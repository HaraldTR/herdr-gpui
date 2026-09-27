---
name: herdr-gpui-browser
description: Show the user a web page in a browser tab of Herdr GPUI, next to the terminal you are running in. Use when you want the user to look at something in a browser, such as a dev server you started, a preview, a pull request, or documentation, and you are running inside a Herdr pane on the user's own machine.
---

# Browser tabs in Herdr GPUI

Herdr GPUI, the native desktop client for Herdr, can show web pages in
browser tabs that sit beside the terminal tabs of a workspace. Open one when
the user should see a page rather than read a URL, for example:

- a local dev server or preview you just started (`http://localhost:3000`)
- a pull request, issue, CI run, or deployment you created
- documentation or a design you are referring to

## Open a page

```sh
herdr-gpui browser open http://localhost:3000
```

- The tab opens in your own workspace (`HERDR_WORKSPACE_ID`, which Herdr sets
  in every pane) and the window switches to it. The terminal keeps running
  underneath; the user returns to it by clicking its tab.
- Add `--no-focus` to add the tab without switching to it, for example when
  the user is typing in your pane.
- Bare hosts work: `localhost:3000` becomes `http://localhost:3000/`, and
  `example.com/docs` becomes `https://example.com/docs`.
- Only `http` and `https` addresses open. `file:` paths and other schemes are
  refused; serve local files over HTTP instead.

Tell the user what you opened and why, in one line. Open a page when it helps
the user, not after every step; each call adds a tab.

## When it does not work

`herdr-gpui browser open` exits with:

| Status | Meaning | What to do |
| --- | --- | --- |
| 0 | Opened | Nothing more |
| 1 | Refused, for example no window shows your workspace | Print the URL for the user instead |
| 2 | Not an http or https address | Fix the address |
| 3 | Herdr GPUI is not running | Print the URL for the user instead |

If the command is not found, or you run on a remote host over SSH, browser
tabs are not available to you: print the URL instead. The app listens only on
the user's own machine.

On Linux the app has no embedded browser yet; the page opens in the system
browser and the command says so.

Run `herdr-gpui browser --help` for the full usage.
