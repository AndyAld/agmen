# 0013: The urgent-alert chime in the desktop app

Status: accepted
Date: 2026-10-26

## Context

Plan Task 13 rings a chime when an urgent ALERT needs the person (spec 8), next to the native notification and the in-app toast. The plan allows a sound synthesized in code or `afplay` on a sound file, with no third-party sound file, and names `assets/sounds/chime.wav` "generated in code at build time or synthesized at startup". The chime must not block the UI thread, must not stack when several alerts arrive together, and must stay silent for history loaded at launch, in tests and in screenshots.

Options checked on 2026-10-26:

- A sound synthesized in code needs an audio output path. `cpal` or `rodio` would be new dependencies. Writing a synthesized WAV to a temporary file and playing it with `afplay` needs no dependency but adds a generator, a temp file and its cleanup for no audible gain.
- `NSSound` through `objc2-app-kit`, which the crate already uses, needs its `NSSound` feature turned on; it would be the first audio code behind our own `objc2` calls.
- `afplay` (`/usr/bin/afplay`, part of macOS) playing a system sound from `/System/Library/Sounds` needs nothing new. macOS ships its alert sounds there (14 on this Mac, `Glass.aiff` among them); they belong to the OS, so agmen ships no sound file.

## Decision

`src/chime.rs` runs `/usr/bin/afplay /System/Library/Sounds/Glass.aiff` as a child process with its output discarded, and returns at once. It keeps the last child and starts no new one while that one still sounds, so alerts that arrive together chime once; a finished child is reaped on the next chime, and a sounding one is killed and reaped when the player is dropped. A player that cannot start is logged to stderr and the alert goes on without sound.

The chime and the notification sit behind the `Alarm` trait in `ui/needs_you.rs`, set as the `AlarmHandle` global by `main.rs` for the app and `--screen` windows. Without the global nothing sounds or posts, which keeps tests silent. Only messages that arrive while the chat is open reach it: history replays through `Arrival::History` and returns no signal. The `needs-you` screenshot shows "chime played" and the toast through `ChatView::show_arrival`, which plays nothing.

The notification uses the toast's title and body ("adam/coord needs you", the alert's text without its leading mentions), through `notify.rs`: `notify-rust` for an unbundled `cargo run` binary, GPUI's `show_system_notification` inside an app bundle.

## Consequences

- No sound file in the repo and no new dependency. There is no `assets/sounds/chime.wav`, unlike the plan's file list.
- The sound is macOS's Glass. Choosing another sound is a path change; a sound setting comes with the Notifications settings page.
- Linux and Windows have no `afplay`; the chime fails quietly there (stderr only). A port picks a player per platform (for example `paplay` with a freedesktop sound, or `PlaySound`) when S8 runs there. Unverified.
- The chime has not been listened to in this task; the tests check that the player and the sound file exist and run `sleep` and `true` in its place.
- The bundled-app notification path (`show_system_notification`) is still untested, as ADR-0010 records; dev runs use `notify-rust`.
