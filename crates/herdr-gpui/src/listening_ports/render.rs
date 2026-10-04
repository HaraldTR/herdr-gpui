//! Drawing a workspace's listening ports: a globe and `:3000 :5173` after
//! it, each number opening its page in a browser tab of that workspace.

use super::Port;
use crate::{config::Theme, usage::Host, window::HerdrWindow};
use gpui::{prelude::*, *};

const GAP: f32 = 6.;

impl HerdrWindow {
    /// The focused workspace's ports in the status bar; nothing while hidden
    /// or while it listens on none.
    pub(crate) fn render_listening_ports(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if !self.config.show_listening_ports {
            return None;
        }
        let endpoint = self.endpoints.get(self.selected_endpoint)?;
        let workspace = self
            .live
            .snapshot
            .as_ref()?
            .focused_workspace_id
            .as_deref()?;
        let daemon = super::Daemon::from(&endpoint.connection.target);
        let ports = self.listening_ports.get(&daemon, workspace);
        if ports.is_empty() {
            return None;
        }
        Some(
            div()
                .id("listening-ports")
                .debug_selector(|| "listening-ports".into())
                .flex_none()
                .h_full()
                .px(px(6.))
                .child(chips(
                    ports,
                    daemon.host(),
                    (&endpoint.id, workspace),
                    &self.theme,
                    14.,
                    cx,
                )),
        )
    }
}

/// A globe and one clickable number per port. `place` is the endpoint and
/// workspace whose browser tab a click opens.
pub(crate) fn chips(
    ports: &[Port],
    host: &Host,
    (endpoint, workspace): (&str, &str),
    theme: &Theme,
    glyph: f32,
    cx: &mut Context<HerdrWindow>,
) -> Div {
    let (muted, foreground, surface) = (theme.muted, theme.foreground, theme.surface);
    div()
        .flex()
        .items_center()
        .gap(px(GAP))
        .overflow_hidden()
        .child(
            svg()
                .path("icons/globe.svg")
                .size(px(glyph))
                .flex_none()
                .text_color(rgb(muted)),
        )
        .children(ports.iter().map(|port| {
            let url = port.url(host);
            let hint = SharedString::from(hint(port, url.as_ref().map(|url| url.as_str())));
            let id = SharedString::from(format!("port-{endpoint}-{workspace}-{}", port.number));
            let chip = div()
                .id(id.clone())
                .debug_selector(|| id.to_string())
                .flex_none()
                .text_color(rgb(muted))
                .child(format!(":{}", port.number))
                .tooltip(move |_, cx| {
                    cx.new(|_| crate::usage::Hint {
                        text: hint.clone(),
                        foreground,
                        surface,
                    })
                    .into()
                });
            match url {
                Some(url) => {
                    let (endpoint, workspace) = (endpoint.to_owned(), workspace.to_owned());
                    chip.cursor_pointer()
                        .hover(|style| style.text_color(rgb(foreground)).underline())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_workspace_page(
                                &endpoint,
                                &workspace,
                                url.clone(),
                                window,
                                cx,
                            );
                        }))
                }
                None => chip,
            }
        }))
}

/// `node listening on *:3000` over what clicking does.
fn hint(port: &Port, url: Option<&str>) -> String {
    let process = if port.process.is_empty() {
        "A process"
    } else {
        port.process.as_str()
    };
    let action = match url {
        Some(url) => format!("Open {url}"),
        None => "Only reachable from the host itself; forward it over SSH to open it".into(),
    };
    format!("{process} listening on {}\n{action}", port.address())
}
