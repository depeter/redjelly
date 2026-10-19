redjelly test build
===================

Install with redjelly-install-<version>.exe (per-user, no admin rights;
Start menu entry, optional desktop shortcut, uninstall via Apps & features).
The zip is the same app as a portable folder.
Logs are in %LOCALAPPDATA%\redjelly\data\logs\redjelly.log (and mpv.log).

Signing in
  First run asks for the server URL. After that you get a "Who's watching?"
  screen: pick a user, use Quick Connect (enter the code in Jellyfin on
  your phone), or "Other user". Your login is saved in Windows Credential
  Manager, so next start goes straight to Home. Ctrl+U switches user.

Browsing (keyboard only, mouse also works)
  Arrows move · Enter opens · Esc/Backspace goes back · Ctrl+H home
  Just start typing (or Ctrl+F) to search · ? shows all keys
  Movies/episodes open a detail page: Resume / Play from start / Mark watched.
  Libraries scroll endlessly; series show every season as a row.

Player keys
  Space/K play-pause    Left/Right 5s    Ctrl+Left/Right 60s   Shift+Left/Right 1s exact
  Up/Down volume        M mute           F fullscreen          , . frame step
  A cycle audio         J cycle subs     V show/hide subs      Z/X sub delay
  [ ] speed             0-9 seek to 0-90%  PgUp/PgDn chapters  I stats overlay
  ? list keys           Esc/Backspace stop
  Mouse: click = pause, double-click = fullscreen, cursor hides while watching.
  Media keys / Windows media overlay control playback too.

  Changing audio or subtitles with A/J is remembered for that show.

Subtitles (S in the player)
  The panel lists results for your languages (nl, en) from:
   - your Jellyfin server's subtitle plugins (free, nothing to set up)
   - Subdl (free): make an account at subdl.com, copy your API key, then add
     to %APPDATA%\redjelly\config\config.toml:
         [providers]
         subdl_api_key = "your-key"
   - OpenSubtitles: OFF by default (daily quota / paid). To enable:
         opensubtitles = true
         opensubtitles_api_key = "..."
         opensubtitles_username = "..."
         opensubtitles_password = "..."   (moved to Windows Credential Manager
                                           on next start, removed from the file)
  Enter downloads: it is auto-synced against another subtitle track of the
  video, loaded right away, and uploaded to Jellyfin for every device.
  When your first language is missing, the search runs by itself at start.
  Z/X delay is remembered per subtitle. Ctrl+Z auto-syncs the current one:
  against another subtitle track if the video has one, otherwise against
  the speech in the audio (takes a minute the first time per video; needs a
  ~10 MB download once). Downloads without another track to align to are
  re-synced to the audio in the background and swapped in automatically.

AI subtitles
  Movie/episode page: "AI Dutch subtitles" button (when Dutch is missing).
  Series page: "AI subtitles: all episodes" queues every episode lacking Dutch.
  In the player: S → "Generate Dutch subtitles from audio" or
  "Translate English subtitle to Dutch". Ctrl+J shows the job list (Enter on
  a running job cancels it). Results are uploaded to Jellyfin and load
  automatically if you're watching that item.
  If an English/Spanish/German subtitle exists it is translated (fast, keeps
  timing); otherwise Whisper transcribes the audio on your NVIDIA GPU.
  Films that mix languages work: every line is translated from its own
  language (English, Spanish, German directly; others via English).
  FIRST USE downloads ~1.5 GB into %LOCALAPPDATA%\redjelly\data\models:
    whisper.cpp CUDA 12 package (~675 MB), Whisper large-v3-turbo (~575 MB),
    English->Dutch translation model (~320 MB).
  Old NVIDIA driver? set in config.toml:  [ai]  whisper_runtime = "cuda11"
  Smaller/faster model:                   [ai]  whisper_model = "medium-q5_0"

Settings (Ctrl+S, or Home → More)
  Languages, skip intro/recap/credits (button / automatic / off), next-episode
  autoplay, subtitle sources and keys, AI engine/model, UI size (for the
  projector; applies after restart), key bindings editor (press any key or
  controller button to assign; Backspace removes), sign out.

Updates
  redjelly checks GitHub for a new version at start and every 12 hours (not
  while you're watching) and shows a message when one is out. Settings ->
  Updates shows what's new; "Install now" downloads it, checks its signature,
  and restarts redjelly on the new version.

While watching
  Enter    skip intro/recap/credits when the button shows; play next episode
           during the countdown (Esc keeps watching)
  N / P    next / previous episode
  Ctrl+S   shader preset: Off → FSR → Anime4K A/B/C (remembered per show;
           downloaded on first use)
  Ctrl+T   HDR tone mapping preset: Automatic / Projector bright (200 nits) /
           Projector dim (100 nits) / Spline (saved; only affects HDR)

Controller (Xbox-style)
  D-pad/left stick move · A select/play-pause · B back · Y subtitles ·
  X audio track · LB/RB seek 10 s · LT/RT seek 60 s · Start menu. Rebind in
  Settings → Key bindings.

Local file test (no server):  redjelly.exe --play "D:\Movies\file.mkv"

Logs:   %LOCALAPPDATA%\redjelly\data\logs\redjelly.log and mpv.log
Config: %APPDATA%\redjelly\config\config.toml (keymap.toml next to it overrides keys)
