//! RDP client backend built on the IronRDP state machines.
//!
//! Unlike IronRDP's reference client, which copies the whole desktop for
//! every graphics update, this driver copies only the updated region into
//! the shared framebuffer, so small updates stay cheap at any resolution.

#[cfg(feature = "audio")]
mod audio;
mod clipboard;

use anyhow::Context;
use clipboard::ClipboardLink;
use ironrdp::connector::{self, ConnectionResult, Credentials, DesktopSize};
use ironrdp::core::WriteBuf;
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::input::{Database, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::gcc::KeyboardType;
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::pdu::rdp::capability_sets::{MajorPlatformType, client_codecs_capabilities};
use ironrdp::pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput, fast_path};
use ironrdp_tokio::{FramedWrite as _, single_sequence_step_read, split_tokio_framed};
use peek_core::{
    ConnectRequest, CursorImage, CursorUpdate, Experience, InputEvent, KeyCode, MouseButton, Rect, SessionCommand,
    SessionContext, SessionEvent, SharedFrame,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

trait AsyncReadWrite: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> AsyncReadWrite for T {}
type UpgradedFramed = ironrdp_tokio::TokioFramed<Box<dyn AsyncReadWrite + Unpin + Send + Sync>>;

/// Kerberos KDC proxying is not supported yet; NTLM works without it.
struct NoNetwork;

impl ironrdp_tokio::NetworkClient for NoNetwork {
    async fn send(
        &mut self,
        _: &ironrdp::connector::sspi::generator::NetworkRequest,
    ) -> connector::ConnectorResult<Vec<u8>> {
        Err(connector::general_err!("Kerberos KDC proxy is not supported"))
    }
}

/// Splits the user name field into (domain, account) for NTLM.
///
/// Accepts `DOMAIN\user`, `user@domain` (UPN) and `MicrosoftAccount\you@outlook.com`.
/// An account that contains `@` is always sent as a UPN with no domain: sspi cannot pair a
/// NetBIOS domain with such an account, and Windows resolves the UPN to the Microsoft account
/// the same way it does at the logon screen.
fn split_user(raw: &str, domain: Option<&str>) -> (Option<String>, String) {
    let (domain, account) = match raw.split_once('\\') {
        Some((d, u)) => (Some(d), u),
        None => (domain, raw),
    };
    let domain = domain.map(str::trim).filter(|d| !d.is_empty() && !account.contains('@'));
    (domain.map(str::to_owned), account.to_owned())
}

fn connector_config(req: &ConnectRequest, width: u16, height: u16) -> connector::Config {
    let creds = &req.credentials;
    let raw_user = creds.username.clone().or_else(|| req.endpoint.username.clone()).unwrap_or_default();
    let (domain, username) = split_user(&raw_user, creds.domain.as_deref());
    let client_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "peek".into());

    connector::Config {
        credentials: Credentials::UsernamePassword { username, password: creds.password.clone().unwrap_or_default() },
        domain,
        enable_tls: true,
        enable_credssp: true,
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_layout: req.options.keyboard_layout,
        keyboard_functional_keys_count: 12,
        ime_file_name: String::new(),
        dig_product_id: String::new(),
        desktop_size: DesktopSize { width, height },
        desktop_scale_factor: req.options.scale_percent,
        bitmap: Some(connector::BitmapConfig {
            lossy_compression: true,
            color_depth: u32::from(req.options.experience.color_depth),
            codecs: client_codecs_capabilities(&[]).expect("default codec list"),
        }),
        client_build: 0,
        client_name,
        client_dir: "C:\\Windows\\System32\\mstscax.dll".to_owned(),
        #[cfg(windows)]
        platform: MajorPlatformType::WINDOWS,
        #[cfg(target_os = "macos")]
        platform: MajorPlatformType::MACINTOSH,
        #[cfg(not(any(windows, target_os = "macos")))]
        platform: MajorPlatformType::UNIX,
        enable_server_pointer: true,
        pointer_software_rendering: false,
        request_data: None,
        autologon: false,
        enable_audio_playback: req.options.audio,
        // XCRUSH, the strongest bulk compressor Windows offers.
        compression_type: req.options.experience.compression.then_some(CompressionType::Rdp61),
        multitransport_flags: None,
        performance_flags: performance_flags(&req.options.experience),
        hardware_id: None,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        alternate_shell: String::new(),
        work_dir: String::new(),
    }
}

/// The Client Info performance flags for an experience: each visual effect is
/// a "disable" flag except font smoothing and composition, which are "enable".
fn performance_flags(e: &Experience) -> PerformanceFlags {
    let mut f = PerformanceFlags::empty();
    let mut set = |on: bool, flag| f.set(flag, on);
    set(!e.wallpaper, PerformanceFlags::DISABLE_WALLPAPER);
    set(!e.window_drag, PerformanceFlags::DISABLE_FULLWINDOWDRAG);
    set(!e.menu_animations, PerformanceFlags::DISABLE_MENUANIMATIONS);
    set(!e.visual_styles, PerformanceFlags::DISABLE_THEMING);
    set(e.font_smoothing, PerformanceFlags::ENABLE_FONT_SMOOTHING);
    set(e.desktop_composition, PerformanceFlags::ENABLE_DESKTOP_COMPOSITION);
    f
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

async fn connect(
    req: &ConnectRequest,
    width: u16,
    height: u16,
    ctx: &SessionContext,
) -> anyhow::Result<(ConnectionResult, UpgradedFramed, Option<ClipboardLink>)> {
    let addr = req.endpoint.socket_addr();
    ctx.events.status(format!("Connecting to {addr}"));
    let stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&addr))
        .await
        .context("connection timed out")?
        .with_context(|| format!("could not reach {addr}"))?;
    stream.set_nodelay(true)?;
    let client_addr = stream.local_addr()?;

    let mut framed = ironrdp_tokio::TokioFramed::new(stream);
    let mut connector = connector::ClientConnector::new(connector_config(req, width, height), client_addr)
        .with_static_channel(
            ironrdp::dvc::DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new()))),
        );

    let clip = if req.options.clipboard && !req.options.view_only {
        let (backend, link) = clipboard::new();
        connector = connector.with_static_channel(ironrdp::cliprdr::CliprdrClient::new(Box::new(backend)));
        Some(link)
    } else {
        None
    };

    #[cfg(feature = "audio")]
    if req.options.audio {
        // Opus below LAN speeds (the Experience compression switch tracks that).
        let sound = audio::SessionSound::new(ctx.audio.clone(), req.options.experience.compression);
        connector = connector.with_static_channel(audio::SoundChannel::new(sound));
        // Windows starts sound output only after the client has announced the
        // device redirection channel; announce it, with no devices.
        let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "peek".into());
        connector =
            connector.with_static_channel(ironrdp::rdpdr::Rdpdr::new(Box::new(ironrdp::rdpdr::NoopRdpdrBackend), name));
        tracing::info!("audio: requesting rdpsnd (with rdpdr)");
    }

    ctx.events.status("Negotiating security");
    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector).await.map_err(rdp_err)?;
    let (initial, leftover) = framed.into_inner();
    let host = req.endpoint.host.as_str();
    let (tls, cert) = ironrdp_tls::upgrade(initial, host).await.context("TLS handshake failed")?;
    {
        use x509_cert::der::Encode as _;
        if let Ok(der) = cert.to_der() {
            ctx.events.send(SessionEvent::ServerIdentity { fingerprint: sha256_hex(&der) });
        }
    }
    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let erased: Box<dyn AsyncReadWrite + Unpin + Send + Sync> = Box::new(tls);
    let mut framed = ironrdp_tokio::TokioFramed::new_with_leftover(erased, leftover);
    let public_key =
        ironrdp_tls::extract_tls_server_public_key(&cert).context("server certificate has no public key")?.to_owned();

    ctx.events.status("Signing in");
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetwork,
        host.into(),
        public_key,
        None,
    )
    .await
    .map_err(rdp_err)?;
    Ok((result, framed, clip))
}

/// Turn IronRDP errors into readable messages.
fn rdp_err(e: connector::ConnectorError) -> anyhow::Error {
    use connector::ConnectorErrorKind as K;
    let msg = match e.kind() {
        K::Credssp(_) => return peek_core::AuthError::Rejected("check the user name and password".into()).into(),
        K::AccessDenied => "access denied: this account may not sign in remotely".to_string(),
        K::Negotiation(f) => format!("the server refused the security negotiation ({f:?})"),
        _ => format!("{}", e.report()),
    };
    anyhow::anyhow!(msg)
}

enum Flow {
    #[allow(dead_code)] // reconnect-on-resize for servers without display control
    Reconnect {
        width: u16,
        height: u16,
    },
    Done(Option<String>),
}

/// Run an RDP session until it ends. Returns the server's disconnect reason
/// when it closed the session gracefully.
pub async fn run(req: ConnectRequest, mut ctx: SessionContext) -> anyhow::Result<Option<String>> {
    let clamp = |v: u32| MonitorLayoutEntry::adjust_display_size(v, v).0 as u16;
    let (mut width, mut height) = (clamp(req.options.width), clamp(req.options.height));
    loop {
        let (result, framed, clip) = connect(&req, width, height, &ctx).await?;
        match active_session(&req, framed, result, clip, &mut ctx).await? {
            Flow::Reconnect { width: w, height: h } => (width, height) = (w, h),
            Flow::Done(reason) => return Ok(reason),
        }
    }
}

/// Copy an updated region of the decoder image into the shared framebuffer.
fn blit(image: &DecodedImage, frame: &SharedFrame, r: &InclusiveRectangle) {
    let (iw, ih) = (image.width() as u32, image.height() as u32);
    let mut w = frame.write();
    if w.fb().width() != iw || w.fb().height() != ih {
        w.resize(iw, ih);
    }
    let rect = Rect::new(
        r.left as u32,
        r.top as u32,
        (r.right as u32 + 1).saturating_sub(r.left as u32),
        (r.bottom as u32 + 1).saturating_sub(r.top as u32),
    )
    .clamp_to(iw, ih);
    if rect.is_empty() {
        return;
    }
    let stride = image.stride();
    let data = image.data();
    let fb = w.fb_mut();
    for y in rect.y..rect.bottom() {
        let start = y as usize * stride + rect.x as usize * 4;
        let src = &data[start..start + rect.w as usize * 4];
        for (d, s) in fb.row_mut(y, rect.x, rect.w).iter_mut().zip(src.as_chunks::<4>().0) {
            *d = u32::from_le_bytes([s[0], s[1], s[2], 0xFF]);
        }
    }
    w.damage(rect);
}

fn scancode(k: KeyCode) -> Scancode {
    let (code, ext) = k.scancode();
    Scancode::from_u8(ext, code)
}

fn to_ops(ev: InputEvent) -> Vec<Operation> {
    let pos = |x: u32, y: u32| MousePosition { x: x.min(u16::MAX as u32) as u16, y: y.min(u16::MAX as u32) as u16 };
    match ev {
        InputEvent::Key { code, down: true } => vec![Operation::KeyPressed(scancode(code))],
        InputEvent::Key { code, down: false } => vec![Operation::KeyReleased(scancode(code))],
        // Physical keys carry the text already; the server applies its layout.
        InputEvent::Text(_) => vec![],
        InputEvent::PointerMove { x, y } => vec![Operation::MouseMove(pos(x, y))],
        InputEvent::Button { button, down, x, y } => {
            let b = match button {
                MouseButton::Left => ironrdp::input::MouseButton::Left,
                MouseButton::Right => ironrdp::input::MouseButton::Right,
                MouseButton::Middle => ironrdp::input::MouseButton::Middle,
                MouseButton::X1 => ironrdp::input::MouseButton::X1,
                MouseButton::X2 => ironrdp::input::MouseButton::X2,
            };
            let op = if down { Operation::MouseButtonPressed(b) } else { Operation::MouseButtonReleased(b) };
            vec![Operation::MouseMove(pos(x, y)), op]
        }
        InputEvent::Wheel { dx, dy, x, y } => {
            let mut ops = vec![Operation::MouseMove(pos(x, y))];
            let clamp = |v: i32| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            if dy != 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: true, rotation_units: clamp(dy) }));
            }
            if dx != 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: false, rotation_units: clamp(dx) }));
            }
            ops
        }
        InputEvent::ReleaseAll => vec![],
    }
}

async fn active_session(
    req: &ConnectRequest,
    framed: UpgradedFramed,
    result: ConnectionResult,
    clip: Option<ClipboardLink>,
    ctx: &mut SessionContext,
) -> anyhow::Result<Flow> {
    let (mut reader, mut writer) = split_tokio_framed(framed);
    let size = result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
    ctx.frame.write().resize(size.width as u32, size.height as u32);
    ctx.events.send(SessionEvent::Connected {
        width: size.width as u32,
        height: size.height as u32,
        detail: "RDP · TLS + NLA".into(),
    });

    let activation_factory = result.activation_factory;
    let mut stage: ActiveStage = ActiveStageBuilder {
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
    let mut input = Database::new();
    let view_only = req.options.view_only;

    let reason = 'outer: loop {
        let outputs = tokio::select! {
            frame = reader.read_pdu() => {
                let (action, payload) = frame.context("connection lost")?;
                ctx.stats.add_in(payload.len());
                stage.process(&mut image, action, &payload).map_err(|e| anyhow::anyhow!("{}", e.report()))?
            }
            cmd = ctx.commands.recv() => match cmd {
                None | Some(SessionCommand::Disconnect) => stage.graceful_shutdown().map_err(|e| anyhow::anyhow!("{e}"))?,
                Some(SessionCommand::Input(_)) | Some(SessionCommand::CtrlAltDel) if view_only => Vec::new(),
                Some(SessionCommand::Input(InputEvent::ReleaseAll)) => {
                    let events = input.release_all();
                    stage.process_fastpath_input(&mut image, &events).map_err(|e| anyhow::anyhow!("{e}"))?
                }
                Some(SessionCommand::Input(ev)) => {
                    let ops = to_ops(ev);
                    if ops.is_empty() { Vec::new() } else {
                        let events = input.apply(ops);
                        stage.process_fastpath_input(&mut image, &events).map_err(|e| anyhow::anyhow!("{e}"))?
                    }
                }
                Some(SessionCommand::CtrlAltDel) => {
                    let keys = [KeyCode::ControlLeft, KeyCode::AltLeft, KeyCode::Delete];
                    let mut ops: Vec<Operation> = keys.iter().map(|&k| Operation::KeyPressed(scancode(k))).collect();
                    ops.extend(keys.iter().rev().map(|&k| Operation::KeyReleased(scancode(k))));
                    let events = input.apply(ops);
                    stage.process_fastpath_input(&mut image, &events).map_err(|e| anyhow::anyhow!("{e}"))?
                }
                Some(SessionCommand::Resize { width, height }) => {
                    let (w, h) = MonitorLayoutEntry::adjust_display_size(width, height);
                    match stage.encode_resize(w, h, Some(req.options.scale_percent), None) {
                        Some(frame) => vec![ActiveStageOutput::ResponseFrame(frame.map_err(|e| anyhow::anyhow!("{e}"))?)],
                        None => {
                            tracing::debug!("display control not available, resize ignored");
                            Vec::new()
                        }
                    }
                }
                Some(SessionCommand::ClipboardText(text)) => match &clip {
                    Some(link) => match link.offer(&mut stage, text).map_err(|e| anyhow::anyhow!("{e}"))? {
                        Some(frame) if !frame.is_empty() => vec![ActiveStageOutput::ResponseFrame(frame)],
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                },
                Some(SessionCommand::ClipboardFiles(paths)) => match &clip {
                    Some(link) => match link.offer_files(&mut stage, &paths).map_err(|e| anyhow::anyhow!("{e}"))? {
                        Some(frame) if !frame.is_empty() => vec![ActiveStageOutput::ResponseFrame(frame)],
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                },
                Some(SessionCommand::Refresh) => Vec::new(),
            },
        };

        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    ctx.stats.add_out(frame.len());
                    writer.write_all(&frame).await.context("connection lost")?;
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    ctx.stats.add_update();
                    blit(&image, &ctx.frame, &region);
                }
                ActiveStageOutput::PointerDefault => ctx.events.send(SessionEvent::Cursor(CursorUpdate::Default)),
                ActiveStageOutput::PointerHidden => ctx.events.send(SessionEvent::Cursor(CursorUpdate::Hidden)),
                ActiveStageOutput::PointerPosition { .. } => {}
                ActiveStageOutput::PointerBitmap(p) => {
                    ctx.events.send(SessionEvent::Cursor(CursorUpdate::Image(Arc::new(CursorImage {
                        width: p.width as u32,
                        height: p.height as u32,
                        hot_x: p.hotspot_x as u32,
                        hot_y: p.hotspot_y as u32,
                        rgba: p.bitmap_data.clone(),
                    }))));
                }
                ActiveStageOutput::DeactivateAll => {
                    // Deactivation-Reactivation: the server changed the desktop (e.g. resize).
                    let mut activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = single_sequence_step_read(&mut reader, &mut activation, &mut buf)
                            .await
                            .map_err(|e| anyhow::anyhow!("reactivation failed: {e}"))?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await.context("connection lost")?;
                        }
                        if let connector::connection_activation::ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                        } = activation.connection_activation_state()
                        {
                            image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
                            ctx.frame.write().resize(desktop_size.width as u32, desktop_size.height as u32);
                            ctx.events.send(SessionEvent::Resized {
                                width: desktop_size.width as u32,
                                height: desktop_size.height as u32,
                            });
                            stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: activation.io_channel_id(),
                                    user_channel_id: activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    bulk_decompressor: None,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            stage.set_enable_server_pointer(enable_server_pointer);
                            break;
                        }
                    }
                }
                ActiveStageOutput::Terminate(reason) => break 'outer reason.description(),
                _ => {}
            }
        }

        if let Some(link) = &clip {
            let (frames, events) = link.drain(&mut stage).map_err(|e| anyhow::anyhow!("{e}"))?;
            for frame in frames {
                ctx.stats.add_out(frame.len());
                writer.write_all(&frame).await.context("connection lost")?;
            }
            for e in events {
                ctx.events.send(e);
            }
        }
    };
    Ok(Flow::Done(Some(reason)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let f = sha256_hex(b"abc");
        assert!(f.starts_with("BA:78:16:BF"));
        assert_eq!(f.len(), 32 * 3 - 1);
    }

    #[test]
    fn domain_user_split() {
        let mut req = ConnectRequest {
            endpoint: peek_core::Endpoint::parse("host").unwrap(),
            credentials: Default::default(),
            options: Default::default(),
        };
        req.credentials.username = Some("CORP\\alice".into());
        let cfg = connector_config(&req, 800, 600);
        assert_eq!(cfg.domain.as_deref(), Some("CORP"));
        match cfg.credentials {
            Credentials::UsernamePassword { username, .. } => assert_eq!(username, "alice"),
            _ => panic!(),
        }
    }

    #[test]
    fn experience_to_flags() {
        use peek_core::ConnectionSpeed;
        let lan = performance_flags(&ConnectionSpeed::Lan.experience());
        assert_eq!(lan, PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION);
        let modem = performance_flags(&ConnectionSpeed::Modem.experience());
        assert!(modem.contains(PerformanceFlags::DISABLE_WALLPAPER | PerformanceFlags::DISABLE_THEMING));
        assert!(
            !modem.intersects(PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION)
        );

        let mut req = ConnectRequest {
            endpoint: peek_core::Endpoint::parse("host").unwrap(),
            credentials: Default::default(),
            options: Default::default(),
        };
        req.options.experience = ConnectionSpeed::Modem.experience();
        let cfg = connector_config(&req, 800, 600);
        assert_eq!(cfg.bitmap.as_ref().map(|b| b.color_depth), Some(16));
        assert_eq!(cfg.compression_type, Some(CompressionType::Rdp61));
    }

    #[test]
    fn user_name_formats() {
        let s = |raw: &str, dom: Option<&str>| split_user(raw, dom);
        assert_eq!(s("CORP\\alice", None), (Some("CORP".into()), "alice".into()));
        assert_eq!(s("alice", Some("CORP")), (Some("CORP".into()), "alice".into()));
        assert_eq!(s("alice@corp.local", None), (None, "alice@corp.local".into()));
        assert_eq!(s("MicrosoftAccount\\you@hotmail.com", None), (None, "you@hotmail.com".into()));
        assert_eq!(s("microsoftaccount\\you@outlook.com", None), (None, "you@outlook.com".into()));
        assert_eq!(s("you@hotmail.com", Some("MicrosoftAccount")), (None, "you@hotmail.com".into()));
        assert_eq!(s("\\alice", None), (None, "alice".into()));
        assert_eq!(s(".\\alice", None), (Some(".".into()), "alice".into()));
    }

    /// Every accepted format must also be accepted by sspi, or CredSSP fails before connecting.
    #[test]
    fn user_names_valid_for_sspi() {
        for raw in ["CORP\\alice", "alice", "alice@corp.local", "MicrosoftAccount\\you@hotmail.com", "\\alice"] {
            let (domain, account) = split_user(raw, None);
            let u = ironrdp::connector::sspi::Username::new(&account, domain.as_deref());
            assert!(u.is_ok(), "{raw}: {u:?}");
        }
    }
}
