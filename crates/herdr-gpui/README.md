# Herdr Native Shell

A GPUI 0.2.2 client for a Local daemon and saved SSH hosts, with macOS support
and experimental Linux x86_64/ARM64 builds.
It starts an installed local `herdr server` when absent; explicit socket and
development targets remain attach-only. It does not link or install Herdr, stop
daemons, spawn a local PTY, or emulate a terminal. Herdr's remote bridge may start
the named remote session. SSH requires an installed POSIX Herdr, noninteractive authentication,
and an already trusted host key.
Runtime dependencies include GPUI, `herdr-client`, `serde_json` for API parameters,
`ureq` for background GitHub owner avatar downloads, and `serde`/`config` (aliased
as `config_loader`, TOML-only) for GUI configuration. `toml` preserves strict
field types during deserialization; `toml_edit` preserves comments on settings saves.

```sh
cargo run -p herdr-gpui
cargo run -p herdr-gpui -- --session default
cargo run -p herdr-gpui -- --session my-project --dev
cargo run -p herdr-gpui -- --socket /absolute/path/to/herdr-client.sock
```

Without flags, discovery follows `herdr-client`'s environment and release-session
rules. `--socket` must name the binary **client** socket, not the JSON API socket.
`--dev` selects the `herdr-dev` config directory. Connection failure is displayed
in the single-row status bar and host rows. Endpoints reconnect independently with
bounded backoff; Terminal > Reconnect retries the selected endpoint immediately,
without input replay. Detach pauses retries for that endpoint until Reconnect.
The status dot pulses amber during local daemon startup, is green when connected,
and red otherwise.

The Rust GitHub updater verifies signed archive manifests and presents a shared
GPUI panel through **app updates** in the sidebar menu or **Herdr > Check for
Updates...**. Background offers change the status version label without taking
focus. Download and **Install and Restart** are separate approvals; closing the
panel does not cancel work. Explicit **Cancel** requests cancellation.
The existing UI timer polls the updater mailbox; workers own blocking work, and
the restart helper is dispatched before CLI parsing or GPUI startup.

Only normal launches with a calendar release version `YYYYMMDD.COUNTER` (tag
`vYYYYMMDD.COUNTER`) and embedded public signing key start the service. Test
fixtures use a disabled, worker-free updater. Update targets are macOS app
bundles and user-owned Linux executables under `HOME` on x86_64/aarch64
GNU systems, not arbitrary packages or a claim of full Linux app support.
**QA > Show app update available** and the sidebar's **preview app update** use
independent synthetic version `9999.0.0`: Download becomes Ready and Install and
Restart only dismisses the panel. No preview action reaches the updater service or quits.
See [update setup and QA](../../docs/updating.md).
The protected release workflow builds both macOS and both Linux architectures
with the required public key. Linux manual `Herdr-VERSION-TARGET.tar.gz` archives
include desktop integration and notices; updater-only
`herdr-gpui-VERSION-TARGET-update.tar.gz` archives contain one executable.
Native two-version update/restart QA remains pending.

Spaces lists Local first, then saved hosts in the upstream catalog's order.
Enabled hosts connect in the background with inactive terminal surfaces; disabled
hosts remain visible. Host and repository collapse state is endpoint-scoped, and
Agents aggregates all connected endpoints with host labels. The catalog is read
through `herdr-client` every two seconds; changes to targets, sessions, enablement,
and ordering are reflected without restarting. Catalog errors preserve the last
valid list. The GUI never edits saved hosts or installs remote software.
An explicit `--socket` is isolated: it never loads or connects saved hosts, or
reads/writes saved selection. Other launches read `client/endpoint-selection.json`
once, under the same release/dev state root as the catalog. Local remains usable
while the desired host connects; restoration waits for its first snapshot rather
than timing out during SSH startup. Explicit host/workspace/agent choices cancel
pending restoration and persist selection asynchronously, without editing hosts.
Other running clients' choices never change this window's selection. Missing,
malformed, disabled or removed saved choices fall back to the valid legacy
catalog selection (normally Local), matching upstream. Live removal/disable
returns to Local and cancels pending restoration; re-enabling does not steal focus.
Automatic activation failure returns to Local without overwriting the saved
preference or repeatedly attempting the same handoff. Write failures are shown
in the status bar and do not undo the UI choice. Host editing remains in
`herdr machine`.

Switching revokes the old host's focus before releasing its surface, then resizes
and activates the selected host. Input waits for the activation acknowledgement
and a coherent surface at the current viewport size. Handoffs time out after five
seconds and return to Local; returning to Local never waits on a remote release.
Servers without surface-switching support remain usable as single targets.

Default and named-session startup discovers Herdr on PATH or in standard
Homebrew, Cargo, or `~/.local/bin` locations, then waits up to 20 seconds to
connect without blocking the UI. If Herdr cannot be found, an installation modal
offers an **Install** button that opens [herdr.dev](https://herdr.dev/); it never
downloads or runs an installer. After installing, choose Terminal > Reconnect.
**QA > Show herdr non-detected modal** previews the warning without restarting,
disconnecting, or changing daemon detection. Closing the GUI leaves the daemon
and its terminals running.

## Configuration

GUI settings live in `$XDG_CONFIG_HOME/herdr/config-gpui.toml`, falling back to
`~/.config/herdr/config-gpui.toml`. See
[`config-gpui.example.toml`](config-gpui.example.toml) for a complete example.
Font sizes use logical pixels (finite 8..48), not typographic points. Restart the
GUI or invoke GUI config reload after edits; daemon config reload is separate.

Preferences has **Theme, Indicators, Sound, Toasts, Integrations, Font, and General**
tabs on one row, horizontally scrollable in narrow windows. Tab/Shift-Tab
switches sections and reveals the selected tab when a font field is not focused.
Font family and size controls save each native font role independently, retaining
configured fallbacks and unrelated settings. Esc leaves a font editor before
closing the modal.

Theme, indicator style, sound, and toast delivery are **shared with the local
Herdr TUI**. They read `HERDR_CONFIG_PATH`, otherwise
`$XDG_CONFIG_HOME/herdr/config.toml` or `~/.config/herdr/config.toml`. The GUI uses
the release `herdr` namespace even in debug builds; select a `herdr-dev` config
explicitly with `HERDR_CONFIG_PATH` when needed. These are local preferences,
not a remote daemon's configuration: a socket does not expose the daemon's config
path or effective settings. General displays the shared file and reload control.

Shared saves preserve comments and unknown keys, reject conflicting external
edits and unsafe paths, and run off the UI thread. Symlinked config files and
user-controlled symlink ancestors are refused rather than replaced. Save success
is separate from the local daemon reload request, which is reported as queued,
not acknowledged. Opening Preferences, its Reload button, and the daemon's reload
signal reread the local file; there is no filesystem watcher.

Existing native theme selections remain overrides. Choose **Follow Herdr** to
use the shared theme, all 18 upstream palettes, custom colors, and automatic
light/dark selection. Selecting a shared theme disables upstream auto-switching,
as in the TUI. Status indicators always use the shared indicator style and shared
status colors, independent of a native terminal theme override. Terminal/default
reset colors are projected to opaque native colors.

Sound respects the shared global and per-agent settings and custom local paths.
On macOS it uses `afplay`; absent or unusable custom audio falls back to the OS
Glass/Ping sounds, not Herdr's bundled MP3s. Herdr toasts render in-app, System
uses macOS notifications (subject to OS permissions and Focus), and Terminal
delivery is not executed in the native app. Native audio/system delivery is not
implemented on Linux. Semantic events are bounded, target-validated, and fenced
by connection/boot; external effects are coordinated across app windows to avoid
duplicate sounds/system notifications. Clipboard feedback remains a separate TUI
preference and does not authorize remote clipboard writes.

Integrations are managed on the **selected daemon's host** through advertised
`integration.list`/`integration.install` methods. Installation is only triggered
by an explicit button click, modifies agent hook/plugin configuration on that
host, and refreshes the list afterward. No uninstall action is offered because
the upstream binary endpoint does not advertise it. Native GitHub sign-in remains
separate from agent integrations.

Set top-level `confirm_close_tab = false` to close tabs without confirmation
(including their running processes), and `show_agents = false` to hide the Agents
section and give Spaces the full sidebar height. Both default to `true`. Pane
closures still ask for confirmation. Reload GUI config or restart after editing.

The `src/config.rs` module exposes `Config::load()` and
`Config::path()`, both returning the crate's typed `Result`. `Config::theme()` resolves
built-ins or Ghostty files into a `Theme` with packed 24-bit RGB colors and all
256 palette entries. Theme resolution is a separate fallible step from loading
and validating TOML. Font sections can override either family or size without
repeating the other field. `FontConfig::line_height()` returns `size * 20 / 14`.
The `[features]` table holds opt-in behaviors as `Features`, with every flag off
by default and unknown keys rejected like the other sections; Preferences lists
each flag and its state read-only, since only the config file turns one on.
Config and theme I/O is synchronous; startup and reload schedule it on the GPUI
background executor and apply the validated pair together. Failed reloads retain
current settings. Theme selection cancels pending reload application so a delayed
load cannot overwrite the newer selection.

Production operations use the root `Error`/`Result` types (`src/error.rs`) with
`thiserror` variants for validation and source-preserving I/O/parser failures.
Catalog channels retain typed errors, and rename results share errors with `Arc`
across cloned UI snapshots. Strings are produced at presentation boundaries, not
as internal error transport. `anyhow` is reserved for framework boundaries and
test harnesses, not internal catch-all errors.
Updater workers and the restart helper use typed `UpdateError` variants, preserving
sources and recovery context while keeping remote diagnostics out of display text.
Active regression tests cover typed sources, redaction, and recovery failures;
the standalone updater harness includes these tests without GPUI dependencies.

## Title Bar

macOS keeps `Some(TitlebarOptions)` and the native Herdr window title/traffic lights,
with transparent chrome and lights positioned at (9, 9) logical pixels. A full-width
34px header blends `theme.surface` roughly 10% toward white, subtly lifting dark
themes while keeping light themes light. It sits above the sidebar and tabs: 80px
of traffic-light clearance, an empty flexible center, and a 40px upper-right slot.
The slot centers a 16px circular user avatar with a 12px SVG in a 28px hover target, tinted from
the theme foreground. This profile control opens native GitHub sign-in and shows
the authenticated user's avatar when connected. It consumes clicks so
double-clicking it does not invoke the title-bar action.
The header and clearance remain in fullscreen so the body layout stays stable.
Windows/Linux keep the existing native frame and do not render this header.

Linked-worktree builds add a full-width, 22px amber banner below
the macOS header (above the body on Linux), with the compile-time branch or short
SHA and optional open PR number, clickable to open that PR on GitHub. The branch
truncates while the PR stays visible. It participates in the root flex
layout, so terminal painting,
hit testing, resize, and IME geometry continue to use the actual canvas bounds.
The banner does not query Git/GitHub or intercept keyboard focus. Main-checkout
builds have no banner. Headless tests cover banner presence/absence, long branch
labels, PR presence/absence, and body bounds at 360px, 640px, and 1200px widths.
Build identity and icon selection are described in the
[release notes](../../scripts/release/README.md#build-identity).

The reference is Zed's `crates/platform_title_bar/src/platform_title_bar.rs` and
window options in `crates/zed/src/zed.rs`, not a build dependency. Double-click calls
`Window::titlebar_double_click()` to honor the OS preference. Unlike newer Zed,
registry GPUI 0.2.2 has no macOS `start_window_move` implementation and ignores
`WindowControlArea::Drag`. We leave `is_movable` unchanged and rely on native AppKit
dragging, rather than adding ineffective custom drag handlers or platform patches.

Headless tests check the actual root header/center/account-slot bounds at wide,
minimum, and narrow sizes, including mock fullscreen entry/exit, and that the
avatar and hit target stay centered. An SVG decoding test checks the embedded user
icon produces a nonempty mask. These do not verify AppKit behavior. Native QA remains
required for dragging across the header, traffic-light alignment and actions,
double-click preferences (zoom/minimize/do nothing), fullscreen transitions and
auto-hidden controls, theme changes, and modal/focus/IME behavior. Windows/Linux
native-frame appearance also remains unverified by these macOS tests.

## Terminal Links

Click an explicit terminal hyperlink or a visible `http://` / `https://` URL to
open it in your default browser. A hand cursor indicates a clickable destination.
Only HTTP and HTTPS destinations are opened. Links inside a popup target that
popup, and menus block activation. Dragging does not activate a link.

Plain URL detection is limited to one row within one pane; links that wrap or
reach the right edge need explicit terminal hyperlink metadata. Other URI schemes
and local file paths are not activated.

## Supported

- Workspace/worktree sidebar with main-checkout parents, indented linked
  workspaces, local collapse arrows, branch details, and daemon-driven
  filled/hollow activity indicators taken from the daemon's own status, so the
  GUI and the terminal client always show the same dot. Each worktree row also
  carries its cached pull request number and diff counts.
- Agents panel header ends with its sort, `grouped` or `priority`, which a
  click flips; an active agent view names itself there instead. Client-local
  and persisted beside the sidebar width, as in the terminal client.
- Resizable sidebar with width persisted per local daemon socket, shared across
  host groups. Local workspace titles show repository owner avatars; remote
  workspaces use the GitHub fallback mark without resolving remote paths locally.
  Profile and owner avatars share a bounded public-image disk cache with 24-hour
  stale-while-refresh behavior; see [avatar caching](../../README.md#native-github-sign-in)
  for limits, location, and the startup authentication requirement. Neither cache
  reads nor downloads block rendering; sign-out discards profile refresh results.
- In-app sidebar menu for settings, keybinds, config reload, update information,
  and detach/reconnect. Tabbed Preferences combines shared Herdr settings,
  daemon agent integrations, editable native fonts, and general configuration.
- A searchable theme picker previews the available names from built-ins and
  Herdr/Ghostty theme folders. Selecting a theme applies and saves it while
  preserving other GUI config settings and comments.
- Right-click spaces for Rename, Close (Close group on non-linked parents with
  multiple spaces sharing `worktree.key`), and New worktree on non-linked Git
  parents. With `features.sidebar_hover_menu` enabled, resting the pointer on a
  space of the selected connection opens the same menu, and moving the pointer
  anywhere but into that menu closes it again; the flag is off by default, so
  spaces normally open their menu only on right-click, and a menu opened by
  right-click stays until it is dismissed. Close requires
  confirmation and terminates terminals, not checkout files or branches. New
  worktree proposes the branch name the daemon would generate, previews the
  checkout path derived from it, reports the daemon's own failures, and selects
  and reveals the created checkout once the daemon reports it. Rename and branch
  dialogs support Unicode/IME, grapheme
  editing, Shift-arrow selection, Home/End, and Cmd-A/C/X/V. Escape/outside click
  cancels; dialog input never reaches terminals or native creation actions.
  Context menus and dialogs anchor to the pointer and clamp to the viewport.
  Rename trims surrounding whitespace and rejects blank labels inline.
- Signed-in workspace menus include a compact, divided PR summary. The number/title
  is the last selectable menu action: click it or use arrows and Enter to open the
   validated URL. Cache-only menu opening shows prefetched results immediately,
   or loading for an initial miss; no separate Open/Refresh controls or O/R shortcuts.
   One background Git/native HTTPS GraphQL worker refreshes eligible Local workspace
   metadata every 90 seconds, with a 128-entry LRU cache, 128 queued jobs, and
   alternating open/focused priority and round-robin scheduling. Failed refreshes
   retain successful data. Ordinary failures back off five minutes; auth/rate-limit
   errors pause the account for an hour by default, honoring numeric retry/reset
   hints within five minutes to 24 hours. Auth/endpoint generations fence late results.
   Discovery uses the daemon repository key and exact branch to resolve a unique
   Git worktree, followed by common-directory/current-branch checks and an explicit
   GitHub repository/head query. It never occupies the deletion dialog response slot.
  On macOS, all socket modes (including explicit/inherited sockets) require a
  same-user kernel peer at the standard configured session socket, with owned,
  non-group/world-writable socket and parent. Executable upgrades/removal do not
  invalidate this local endpoint trust. SSH and sockets elsewhere remain blocked;
  a same-user proxy deliberately replacing the trusted socket is not detectable.
  Reconnect rechecks the endpoint. See
  [PR lookup scope and limits](../../README.md) for authentication and remote limits.
   The same worktree-registry path supports both current and older daemons without
   `workspace.get`. No Git or HTTP requests run from menu-open or render paths.
- The top-right titlebar profile control starts native GitHub device sign-in on
  a signed-out click, shows the authenticated user's avatar, and offers Sign out
  on right-click. Signed-out workspace menus have no GitHub section or requests.
  It uses Herdr GPUI's public client ID `Iv23liurUcwxPjrdIFYT`, overridden by
  `[github].oauth_client_id`, then `HERDR_GITHUB_OAUTH_CLIENT_ID`. No client secret
  or private key is needed or shipped. The compact native macOS titlebar design
  is integrated from main commit `3909f21`, without unrelated tab changes.
  Signed macOS release builds keep tokens in this app's Keychain entry; unsigned
  development and worktree builds use the private file store instead, so a new
  code identity per rebuild cannot trigger a Keychain prompt on every launch.
  `GH_TOKEN` / `GITHUB_TOKEN` override either. Access tokens and retained device/user codes use redacted, zeroizing
  `secrecy` types; HTTP headers are sensitive and application-owned raw OAuth
  buffers are wiped. The user code is intentionally exposed for rendering.
  Library/OS/rendering copies are not guaranteed to be erased. Linux supports an
  explicit `allow_plaintext_credentials = true` opt-in with a prominent warning,
  separate private credential file and atomic no-follow Unix writes; macOS
  development builds use that same store, enabled by default and warned about in
  the profile panel. Signed macOS release builds still use Keychain. Sign-out suppresses environment tokens for this app session and
  fences late profile/avatar/PR results. Plaintext policy reloads re-evaluate the
  active credential, without reactivating an explicitly signed-out session.
  Disabling plaintext stops its session use but keeps the file; explicit sign-out
  still removes the saved file regardless of opt-in, or reports a safe error.
  Token refresh is not implemented; an
  expired GitHub App token requires reauthentication. No CLI authentication is used. See
  [setup, cancellation, scopes, and sign-out](../../README.md#native-github-sign-in).
- Workspace actions retain the clicked ID and boot, revalidate before queueing,
  and reject changed close-group membership. Reconnect clears dialogs. Queue
  errors remain in the dialog; daemon errors appear in the connection status bar.
  Queue acceptance dismisses the dialog, not an optimistic state mutation.
- Linked spaces offer Delete worktree checkout with a daemon-resolved path and a
  single confirmation, matching the Herdr TUI. Unlike other workspace dialogs,
  deletion stays open until the correlated daemon result arrives. Dirty/untracked
  refusals and errors are shown inline; force requires a new confirmation. All Git/filesystem work
  stays in Herdr. Unpushed commits are not checked by this API. See
  [WORKTREE-DELETION.md](WORKTREE-DELETION.md) for safety limits and sources.
- Worktree creation sends the clicked `workspace_id`, optional `branch`,
  `base: "HEAD"`, `focus: true`, and `trust_repository: false`. Blank branches use
  daemon policy. The daemon's deferred endpoint navigation focuses the result;
  no follow-up focus request or local Git subprocess is used.
- Title-only tabs, without an added tab number. Externally created workspaces
  arrive through pushed snapshots without manual refresh.
- Right-click any tab without focusing it to open Rename.
  Actions retain the clicked tab/workspace and reject stale connections or targets.
  Rename selects the current label in a native IME-aware field, with inline errors;
  Close uses the existing cancel-by-default confirmation unless
  `confirm_close_tab = false`. Escape or an outside left/right click dismisses
  the menu without sending terminal input.
- Click workspace, tab, agent, or a visible split pane to focus through the API.
- Native File/Terminal menus and creation buttons: **+ New Workspace** in the
  sidebar and a persistent 18px SVG **+** in a 44px-wide button beside the horizontally
  scrolling tab strip. Each tab has a 16px SVG close cross in a 24px hit target;
  it uses the same configurable confirmation without focusing an inactive tab.
  Both icons use the current theme's foreground tint.
- Cmd-N creates and focuses a workspace; Cmd-T creates and focuses a tab.
  Cmd-D splits the focused pane vertically (new pane on the right);
  Cmd-Shift-D splits horizontally (new pane below). Cmd-Shift-] / Cmd-Shift-[
  cycles next/previous tab within the current workspace, wrapping at the ends.
  These shortcuts are native actions, not bytes sent to a terminal.
- Cmd-1 through Cmd-9 focuses the corresponding numbered tab in the current
  workspace. Cmd-Alt-Left/Right/Up/Down focuses a pane in that direction;
  Cmd-Alt-] / Cmd-Alt-[ cycles next/previous pane within the current tab.
  Cmd-Shift-Enter toggles focused pane zoom.
- Cmd-W closes the focused pane and Cmd-Shift-W closes the focused tab only after
  a confirmation dialog (tab confirmation can be disabled with
  `confirm_close_tab = false`). **Cancel is selected by default**: Enter alone cancels;
  Tab then Enter selects and confirms Close. Closing can terminate running
  processes, unlike quitting the GUI, which only detaches.
- Cmd-Shift-P opens the command palette with native actions and configured daemon
  command entries, including native Themes and Reconnect actions without dedicated
  shortcuts. Cmd-P opens the workspace picker instead.
- Cmd-B toggles sidebar visibility locally without changing daemon state.
  Cmd-, opens Settings; Cmd-/ opens the grouped native shortcut reference.
  Native shortcut labels and keycaps come from the shared `controls::COMMANDS`
  catalog, with Cmd-V semantic paste shown separately. Search filters by action,
  section, or key combination. Preferences, keybinds, theme/palette pickers, and
  close confirmations use themed centered modals and configured UI fonts;
  modal input does not reach the terminal.
- Creation omits `cwd`, labels, environment overrides, and split ratio: the
  daemon applies its existing defaults and directory policy. Workspace creation
  supplies the currently focused source workspace when available; tabs and splits
  target the current workspace/pane explicitly. An empty session can create a
  workspace without guessing a local path. Nothing is created while disconnected.
- Vertical mouse-wheel/trackpad scrolling targets the pane under the pointer
  (inside its content, not borders). Fractional pixel motion accumulates into
  terminal lines, with bounded per-event work. Popups capture wheel input only
  within their displayed bounds; input never falls through to a covered pane.
- Direct semantic cell canvas: named ANSI colors, indexed 256-color palette,
  RGB, reset foreground/background, reverse, dim, hidden, bold, italic,
  underline, strikeout, wide-cell skip handling, and cursor shapes.
- Server popup text surfaces centered above the main surface, with popup input
  routing while one is active.
- Native committed text through `EntityInputHandler`, including Unicode and
  composition. In-progress marked text is shown in the status bar.
- Enter, Tab/BackTab, Escape, Backspace, arrows, navigation/editing keys,
  F1-F24, Control characters and modifiers on special keys. Option-printable
  input follows the macOS keyboard layout, including dead keys.
- Cmd-V sends semantic Paste; Cmd-Q or window close detaches without killing
  the daemon or its terminals. Window activation is reported to the daemon.
- Resize uses the actual terminal canvas bounds and measured configured font cell width,
  excluding the native sidebar, tabs and status bar.

Socket I/O belongs to `herdr-client`'s worker. A separate event thread drains all
ordered events into a bounded latest-state cache. The UI samples changed state
at most once per 16 ms without blocking. Snapshots invalidate surfaces with a
different boot/projection revision; input waits for a coherent surface. Reconnect
replaces the cache, so late events from an old connection cannot affect the UI.

### Scrolling Semantics

Upstream `PaneScrollParams` is exactly `{ "pane_id": string,
"offset_from_bottom": u64 }`, an absolute scrollback position, not a wheel delta.
Like the upstream TUI's normal wheel handling, this GUI instead sends semantic
`ClientPaneInputEvent::Mouse` (`ScrollUp`/`ScrollDown`, pane-relative position,
modifiers, and line count). The daemon's `apply_scroll` chooses host scrollback,
alternate-screen behavior, or application mouse reporting using the current
terminal mode. This avoids racing absolute `pane.scroll` offsets against incoming
frames and avoids duplicating terminal-mode policy in the GUI. Scrolling does not
change keyboard focus to the hovered pane. The existing client advertises no pixel
mouse capability, so the daemon uses the supplied cell-coordinate fallback.

Reference sources (read-only): Herdr's `src/api/schema/{workspaces,tabs,panes}.rs`,
`src/app/api/workspaces.rs`, `src/client/shell/mouse.rs`, and
`src/server/pane_input.rs`; Arbor's `crates/arbor-gui/src/app_bootstrap.rs` for
GPUI native action/menu/keybinding patterns.

## Deliberate Limitations

- macOS defaults to Menlo and the system font; Linux defaults to DejaVu Sans Mono
  and DejaVu Sans. No bundled Nerd Font.
  Private-use icons may be missing. Fonts and palettes are configured locally,
  not synchronized from the host terminal's theme.
- No draggable scrollback UI, text selection/copy, mouse button/motion reporting, split dragging,
  image rendering, or animated blinking.
- No pane rename dialogs or horizontal wheel handling,
  server-owned keybindings, session picker, saved-host editing, or daemon
  stop/upgrade management.
- IME uses a minimal transient buffer, not a local editable terminal document;
  composition appears in the status bar rather than inline. Key releases and
  physical-key/extended keyboard protocol metadata are not reported.
- Popups have a basic centered text presentation, without native title/border
  chrome. Only semantic notifications are delivered under local sound/toast
  policy; legacy terminal escapes and remote clipboard writes are not executed.
- Rendering is a simple two-pass cell painter, not an optimized damaged-row
  renderer. Large/high-frequency surfaces can consume significant CPU.

## Build And Test

```sh
cargo check -p herdr-gpui
cargo test -p herdr-gpui
cargo clippy -p herdr-gpui --all-targets -- -D warnings
cargo fmt -p herdr-gpui -- --check
```

Requires the normal macOS Rust/Xcode development environment or the
[Linux build dependencies](../../README.md#linux-builds). Registry GPUI's default
X11/Wayland backends are retained. Linux uses Vulkan; macOS GPUI's
`runtime_shaders` feature compiles native Metal shaders at app launch, avoiding
the separate downloadable build-time Metal compiler. Integrated Linux ARM64
compilation, Clippy, default/all-feature tests, and release CLI checks were
verified in Ubuntu 24.04, not native desktop rendering or input. Linux Cmd bindings mean
Super and can conflict with desktop shortcuts; global macOS menus are not
available. Tests cover wire colors,
cell modifiers, viewport bounds, semantic key selection, revision coherence,
creation request parameters, workspace-local tab cycling, wheel accumulation,
pane-relative hit testing, and popup routing.
Workspace-menu regressions check clicked-target schemas, stale boot/group
rejection, Unicode composition, and headless right-click/input routing.
`just test-sidebar` additionally exercises the native dialogs at narrow and wide
sizes, but does not validate OS IME candidate-window delivery or live daemon
worktree creation/close.
They do not replace an interactive smoke test against a live daemon.

`just test-sidebar` runs isolated, daemon-free native fixtures on the active
desktop. On macOS it checks exact-window clicks with a decoy key window, host
selection/disabled hosts, scoped collapse, duplicate-ID navigation routing,
composition preservation, menu isolation, and long-label native glyph clipping.
Scroll independence uses scroll handles and native draws, not trackpad events.
See the root README for the full verification scope and remaining limitations.
