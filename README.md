# iris

iris shows the GitHub Actions runs that wait for your deployment approval on
your desktop. You approve or reject them in the terminal TUI. One API call
reviews every environment of a run. The banner never approves. You approve in
the TUI or on the website.

`homelab-iac` gates each apply with a GitHub environment and a required
reviewer. One push can leave up to four runs waiting. iris makes those waits
visible on the desktop.

The review comment ("approved with iris") shows in the deployment history.

## Install

You need `cargo`, `gh`, and a D-Bus session bus.

```sh
cargo build --release
install -m755 target/release/iris ~/.local/bin/iris
```

iris calls the GitHub REST API through `gh`. Log in once:

```sh
gh auth login
```

iris stores no token. It uses your `gh` login.

## Use

| Command | Works |
| --- | --- |
| `iris` | Open the TUI. |
| `iris list` | Print the runs that wait for approval. |
| `iris daemon` | Poll the repository and show banners. |
| `iris daemon --once` | Run one poll and stop. |
| `iris notify-test` | Show one test banner. |

### The TUI section

| Section | Content |
| --- | --- |
| `[1] waiting` | The runs that wait for your approval. |
| `[2] recent` | The newest runs of the repository. |

### The TUI keys

| Keys | Action |
| --- | --- |
| `j` `k` | Move in the list. |
| `h` `l` | Change the section. |
| `a` | Approve all environments of the run. |
| `R` | Reject all environments of the run. |
| `Enter` | Open the review of the run. |
| `space` | Mark or unmark an environment. |
| `x` | Reject the marked environments. |
| `o` | Open the run in the browser. |
| `c` | Cancel the run. |
| `r` | Rerun the run. |
| `f` | Rerun the failed jobs of the run. |
| `g` | Refresh now. |
| `?` | Show the key map. |
| `q` | Quit. |

### The banner

One banner shows each waiting run and its environments. The banner gives
information only. Its button opens GitHub.

A banner that you do not answer shows again after the re-ping interval. The
new banner replaces the old one. When a run that iris showed completes, iris
shows a second banner with the result.

## Configure

You set each value with an environment variable. Each variable has a default.

| Variable | Default | What it sets |
| --- | --- | --- |
| `IRIS_REPO` | `cnbrown04/homelab-iac` | the repository to watch |
| `IRIS_POLL_SECS` | `60` | seconds between the polls of the daemon |
| `IRIS_REPING_SECS` | `600` | seconds before an ignored banner shows again |
| `IRIS_AUTO_SECS` | `30` | seconds between the refreshes of the TUI |
| `IRIS_NOTIFY_MS` | `15000` | milliseconds one banner stays |
| `IRIS_TITLE_MAX` | `70` | the max characters of a commit title |
| `IRIS_CACHE` | `~/.cache/iris` | the state directory |

The state lives in the cache directory. Each watched run gets two small
files. iris deletes the old entries.

## The daemon at login

Start the daemon with a systemd user unit:

```sh
systemctl --user enable --now iris.service
```

Example unit at `~/.config/systemd/user/iris.service`:

```ini
[Unit]
Description=iris: GitHub Actions deployment approval watcher

[Service]
Type=simple
ExecStart=%h/.local/bin/iris daemon
Environment=DBUS_SESSION_BUS_ADDRESS=unix:path=%t/bus
Restart=on-failure
RestartSec=15

[Install]
WantedBy=default.target
```

The unit must set `DBUS_SESSION_BUS_ADDRESS`. A user service does not get the
environment of the desktop session. Show the logs with:

```sh
journalctl --user -u iris -f
```

Optional: add a mise task, so `mise run approve` opens the TUI from any
directory. Put this in `~/.config/mise/tasks/approve`:

```bash
#!/usr/bin/env bash
exec "$HOME/.local/bin/iris" "$@"
```

## Remove

```sh
systemctl --user disable --now iris.service
rm ~/.config/systemd/user/iris.service
rm ~/.local/bin/iris
rm -r ~/.cache/iris
```

## Notes

- iris sends the banners over D-Bus. Use any standard notification server.
- An environment that blocks self-approval shows as "cannot approve". iris
  does not send those environments to the review.
- The current state: tested with Hyprland and quickshell on one desktop.
