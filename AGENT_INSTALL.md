# AI Agent Install Guide

This file is written for AI coding agents installing Codex Usage Tray for a user.

## Goal

Install the latest GitHub Release of `codex-linux-usage-tray-indicator` as a native Linux tray app.

Repository:

```text
https://github.com/wojciechsacewicz/codex-linux-usage-tray-indicator
```

## What The App Does

After installation, explain to the user that Codex Usage Tray:

- adds a native Linux tray/status-notifier item;
- reads local Codex session JSONL files from `$CODEX_HOME` or `$HOME/.codex`;
- shows 5-hour and weekly rate-limit usage;
- shows reset countdowns and local reset times;
- estimates token usage and API-equivalent cost;
- opens a local Details dashboard with model breakdowns;
- supports configurable refresh intervals;
- can show optional fullscreen confetti when a rate-limit window resets;
- does not send usage data anywhere.

## Requirements

The machine must have the native runtime libraries installed:

- GTK3
- Ayatana AppIndicator
- GTK Layer Shell

On Arch Linux:

```bash
sudo pacman -S gtk3 libayatana-appindicator gtk-layer-shell
```

On Debian/Ubuntu:

```bash
sudo apt install libgtk-3-0 libayatana-appindicator3-1 libgtk-layer-shell0
```

Package names vary by distribution. If install fails because a shared library is missing, install the matching distro package and retry.

## Download Latest Release

Use GitHub CLI when available:

```bash
tmpdir="$(mktemp -d)"
cd "$tmpdir"
gh release download --repo wojciechsacewicz/codex-linux-usage-tray-indicator --pattern '*x86_64-linux.tar.gz'
tar -xzf ./*.tar.gz
```

Without GitHub CLI, query the latest release URL and download the `x86_64-linux.tar.gz` asset with `curl`.

## Install

Run:

```bash
./install.sh
```

This installs:

```text
~/.local/bin/codex-usage-tray
~/.local/share/applications/codex-usage-tray.desktop
```

Ensure `~/.local/bin` is on `PATH`.

## Start The App

Run:

```bash
codex-usage-tray
```

For a desktop session, prefer launching from the application menu or with:

```bash
setsid -f ~/.local/bin/codex-usage-tray
```

## Optional Autostart

To start on login:

```bash
mkdir -p ~/.config/autostart
cp ~/.local/share/applications/codex-usage-tray.desktop ~/.config/autostart/
```

## Verify

Check that the process is running:

```bash
pgrep -af codex-usage-tray
```

If the tray icon is not visible, tell the user that their desktop shell or panel must support AppIndicator/status-notifier items.

## What To Tell The User After Installing

Use a concise summary like:

```text
Codex Usage Tray is installed and running. It adds a native Linux tray item that shows Codex rate limits, reset times, token usage, and estimated API-equivalent cost from your local Codex session files. Use Details for the dashboard, Refresh now for an immediate update, and the settings/menu controls for refresh interval and party mode. It reads local files only and does not send usage data anywhere.
```

Mention any caveats that applied during installation, especially missing desktop tray support or missing native libraries.
