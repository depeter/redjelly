# redjelly — specification

A native Rust desktop client for Jellyfin. Windows is the primary target; Linux is
supported (and is the dev platform). Goal: replace Jellyfin Media Player entirely,
fixing its weak spots — keyboard focus, subtitle handling, startup/browse
performance, and playback/track-selection quirks.

Status: v1 spec, agreed 2026-10-15. All milestones are in scope for v1.

---

## 1. Decisions (from spec interview)

| Topic | Decision |
|---|---|
| Scope | Full client: login, browse, search, continue watching, next up, playback, progress sync |
| Playback engine | libmpv (`libmpv2` crate) |
| UI toolkit | Slint (femtovg/OpenGL renderer), mpv drawn as an OpenGL underlay |
| Platforms | Windows first; Linux supported. Platform specifics behind traits |
| Keyboard | Full UI navigation by keyboard, mpv-style player hotkeys, media keys + SMTC |
| Rebinding | TOML keymap with defaults + in-app "press a key" editor + `?` cheat sheet |
| Gamepad | Yes, v1 (XInput via `gilrs`) |
| Sub providers | Jellyfin server remote search, Subdl (free); OpenSubtitles.com opt-in (quota/paid). Podnapisi dropped: site gone (no DNS, 2026-10) |
| Sub storage | Upload to Jellyfin server (user is admin) |
| Auto-fetch | Background search when preferred-language sub missing; toast, no auto-download |
| Sub tools | Manual delay (remembered), auto-sync via alass, style overrides |
| AI subs | Local whisper.cpp (CUDA, NVIDIA GPU), on demand — run as the official prebuilt `whisper-cli` (CUDA 12 package downloaded on first use), since CUDA can't be cross-compiled from Linux |
| Translation | Local model (Opus-MT via candle), e.g. en→nl |
| Languages | Ordered preference list, user default `nl, en` |
| Track rule | Audio by preference list; subtitles always on, by preference list |
| NL fallback | Offer Whisper + translate when no provider has a preferred-language sub |
| Servers | One server, multiple users (quick switch) |
| Server | Jellyfin 10.10+ (user runs 10.11), user is admin |
| Display | Laser projector on a large canvas. HDR is tone-mapped to SDR by mpv (no HDR passthrough) |
| Out of v1 | Offline downloads, Live TV, music, trickplay, SyncPlay, Discord presence, multi-server |

---

## 2. Architecture

```
app (redjelly.exe, Slint UI, tokio runtime)
 ├── rj-core      config, language codes, track-selection rules, local state DB
 ├── rj-jellyfin  typed REST client for the Jellyfin API
 ├── rj-player    libmpv wrapper: playback, tracks, OSD, render-API integration
 ├── rj-subs      subtitle providers, ranking, hashing, encoding fixups, alass sync, upload
 ├── rj-ai        audio extraction, whisper transcription, Opus-MT translation, job queue
 └── rj-input     Action model, keymaps, keyboard/gamepad/media-key sources
```

Rules:
- `rj-core` is pure logic, no I/O beyond config/DB files — heavily unit-tested.
- All input sources (keyboard, gamepad, SMTC/MPRIS) translate to `rj_input::Action`.
  The UI and player only ever handle Actions — never raw keys.
- Network and heavy work run on tokio; UI updates are posted back via
  `slint::invoke_from_event_loop`. The UI thread never blocks.
- Heavy deps (`whisper-rs` + CUDA, `candle`) are cargo features of `rj-ai` so the
  default build stays fast.

### 2.1 Video rendering
mpv's render API (OpenGL) draws into the Slint window as an underlay using
`Window::set_rendering_notifier` (BeforeRendering); Slint draws the OSD/UI on top.
This keeps one window, one focus model, and real overlays (no child-HWND z-order
problems).

**Spike M0 — passed 2026-10-15** on Windows/NVIDIA: `nvdec` hwdec, 4K HEVC HDR10
tone-mapped correctly, ~1 dropped frame, UI composited over video.

**HDR:** always tone-mapped to SDR by mpv. The user watches on a projector, where
player-side tone-mapping usually beats the projector's own HDR handling, and the
OpenGL underlay can't output HDR anyway. HDR passthrough is out of scope.

### 2.2 Local data
- Config: `%APPDATA%\redjelly\config.toml`, keymap: `keymap.toml` (Linux: XDG dirs).
- State (`state.json` for now; move to SQLite if it grows): per-series track memory,
  per-item subtitle delay, AI job records, provider quota cache.
- Image cache: on-disk, keyed by item id + image tag (tags make it immutable).
- Credentials: access tokens in Windows Credential Manager / Secret Service (`keyring`).

---

## 3. Jellyfin integration

Auth header on every request:
`Authorization: MediaBrowser Client="redjelly", Device="<host>", DeviceId="<uuid>", Version="<ver>", Token="<token>"`

| Feature | Endpoint |
|---|---|
| Login | `POST /Users/AuthenticateByName`, Quick Connect (`/QuickConnect/*`) |
| Public users (switcher) | `GET /Users/Public` |
| Home rows | `GET /UserViews`, `/UserItems/Resume`, `/Shows/NextUp`, `/Items/Latest` |
| Browse/search | `GET /Items` (paged, sort/filter), `/Shows/{id}/Seasons`, `/Shows/{id}/Episodes` |
| Playback info | `POST /Items/{id}/PlaybackInfo` with a device profile that declares everything direct-playable |
| Stream | `/Videos/{id}/stream?static=true&mediaSourceId=…` (token via mpv `http-header-fields`) |
| Reporting | `POST /Sessions/Playing`, `/Sessions/Playing/Progress` (every 10 s + on pause/seek/track change), `/Sessions/Playing/Stopped` |
| External subs | `/Videos/{id}/{msid}/Subtitles/{index}/Stream.{fmt}` → mpv `sub-add` |
| Server sub search | `GET /Items/{id}/RemoteSearch/Subtitles/{lang}`, `POST /Items/{id}/RemoteSearch/Subtitles/{subId}` |
| Sub upload | `POST /Videos/{id}/Subtitles` `{Language, Format, IsForced, IsHearingImpaired, Data(base64)}` |
| Intro/credits | `GET /MediaSegments/{id}` (Intro, Outro, Recap, Preview, Commercial) |

**Direct play is mandatory.** The device profile claims support for all containers and
codecs; transcoding happens only if the server refuses (bitrate limit) — then show
why in the OSD. Track switching is always local in mpv, never a server transcode.

**Track index mapping:** Jellyfin `MediaStream.Index` ≠ mpv track id. Embedded tracks
map by order within type; external server subs are added with `sub-add` and their mpv
id recorded. This mapping lives in `rj-player` and is unit-tested.

---

## 4. Keyboard, gamepad, media keys

### 4.1 Focus model (fixes JMP's lost-focus bug)
- Every screen is a grid of focusable rows/cards. There is **always** exactly one
  focused element; when the focused element disappears, focus moves to the nearest
  neighbour (never to nothing).
- Visible focus ring everywhere. Arrow keys = spatial navigation, Enter = activate,
  Esc/Backspace = back, Tab/Shift+Tab = next/previous region.
- Focus is restored when returning to a screen (remember per-screen focus path).
- Type-to-search: typing a letter on a browse screen opens search with that letter.

### 4.2 Default player keys (all rebindable)

| Key | Action | Key | Action |
|---|---|---|---|
| Space / K | play/pause | F / Alt+Enter | fullscreen |
| ← / → | seek −5/+5 s | Ctrl+← / Ctrl+→ | seek −60/+60 s |
| Shift+← / Shift+→ | exact seek −1/+1 s | , / . | frame step back/forward |
| ↑ / ↓ | volume ±5 | M | mute |
| PgUp / PgDn | prev/next chapter | N / P | next/previous episode |
| [ / ] | speed −/+0.1 | Shift+Backspace | reset speed |
| A | cycle audio track | J / Shift+J | cycle subtitle / reverse |
| V | toggle subtitles | S | open subtitle panel (search/download/AI) |
| Z / X | sub delay −/+100 ms | Ctrl+Z | auto-sync subtitle (alass) |
| Enter | skip intro/credits (when shown) | I | stats overlay |
| Esc / Backspace | hide controls → stop → back | ? | keybinding cheat sheet |
| 0–9 | seek to 0%–90% | Ctrl+S | cycle shader preset |

### 4.3 Keymap file
`keymap.toml`, sections by context (`global`, `browse`, `player`, `dialog`), value is
an action name or a list of action names. Unknown keys/actions → warning, default
kept. The in-app editor writes the same file. Conflicts within one context are
rejected by the editor.

### 4.4 Media keys / SMTC
Via `souvlaki`: SMTC on Windows (media keys, volume flyout, lock screen; title,
series, artwork, position), MPRIS on Linux. Events → Actions.

### 4.5 Gamepad (`gilrs`)
D-pad/left stick = navigate, A = activate/play-pause, B = back, X = audio menu,
Y = subtitle panel, LB/RB = seek ∓10 s, LT/RT = seek ∓60 s, Start = OSD menu,
Back/View = stats. Rebindable in the same keymap (`gamepad` section).

---

## 5. Playback behaviour

### 5.1 Track selection (`rj_core::tracks`)
Inputs: stream list, preference list (`["nl", "en"]`), per-series memory, settings.

**Audio:** if a per-series remembered language exists and is present → use it. Else
first audio track whose language matches the preference list in order (ties: default
flag, then most channels, then lowest index). No match → the track flagged default →
first track.

**Subtitles: always on.** Per-series memory first, else first preference language
that has a sub. Within a language: full (non-forced) over forced; non-SDH over SDH
(setting `prefer_sdh` flips it); text (SRT/ASS/VTT) over image (PGS/VobSub) for
styleability; then default flag, then lowest index. No preferred-language sub at all →
subtitles off.

**Auto-search trigger:** if the *top* preference language (nl) is not present, start a
background provider search (§6.3), even when the en fallback was selected.

Language codes are normalised (`nl`/`nld`/`dut`/`Dutch` → `nl`) — Jellyfin reports
ISO 639-2 B codes, providers use 639-1.

### 5.2 Track memory
Manually changing audio/subtitle language during playback stores that language for
the series (or the movie's collection, else the movie), not the track index. Cleared
from the item's context menu.

### 5.3 Other
- Resume from server position; "resume / start over" prompt only when position > 5%.
- Intro/Recap/Credits: segment button ("Skip intro", Enter); per-type setting
  `ask | auto | off` (default: intro ask, recap auto, outro ask).
- Next episode: on Outro segment or last 30 s, show "Next episode in 10 s" card;
  Enter = now, Esc = cancel. Setting for countdown length / off.
- Shader presets: off, FSR, Anime4K (Mode A/B/C), configurable list of glsl files;
  per-library default (e.g. anime library → Anime4K).
- Stats overlay: direct play vs transcode, codec, hwdec, dropped frames, bitrate.
- Tone-mapping settings for HDR sources (projector tuning): `target-peak`,
  `tone-mapping` algorithm (`bt.2390`, `spline`, `mobius`…), `hdr-compute-peak`,
  `tone-mapping-param`; named presets, cycled with a key during playback and shown
  in the stats overlay.
- UI scale setting for 10-foot viewing (projector/TV at couch distance):
  absolute `SLINT_SCALE_FACTOR` (or follow Windows scaling), applied on restart.
- Shader presets per show (Ctrl+S): FSR (pinned gist) and Anime4K v4.0.1 modes
  A/B/C, downloaded on first use. Tone-mapping presets (Ctrl+T) in config.

---

## 6. Subtitles

### 6.1 Providers (`rj_subs::Provider` trait)
```rust
trait Provider {
    fn id(&self) -> ProviderId;
    async fn search(&self, q: &SubtitleQuery) -> Result<Vec<SubtitleCandidate>>;
    async fn download(&self, c: &SubtitleCandidate) -> Result<SubtitleFile>;
}
```
- **OpenSubtitles.com** REST v1 — **off by default** (user prefers to avoid it: downloads
  use a small daily quota, more costs money; searching is free). Needs the user's own
  API key + login (token cached); always ranked below free providers. Query by `moviehash` + `imdb_id`/`tmdb_id`/`parent_imdb_id` +
  season/episode. Show remaining daily quota in the panel.
- **Jellyfin server** remote search (uses whatever plugins the server has). The server
  downloads and stores the sub itself — no upload step needed.
- **Subdl** (free API key from a subdl.com account). ZIP downloads, season packs
  resolved by SxxEyy.

Query built from Jellyfin metadata (`ProviderIds` Imdb/Tmdb/Tvdb, series ids,
season/episode, year, original file name from `MediaSource.Path`).

**Movie hash** (OpenSubtitles algorithm: size + checksum of first/last 64 KiB) is
computed remotely with two HTTP Range requests on the static stream — no full
download.

### 6.2 Ranking
0. Free provider before quota-limited (OpenSubtitles). 1. Hash match. 2. Release-name similarity to the file name (source: BluRay/WEB-DL,
group, resolution, edition). 3. Not hearing-impaired (unless `prefer_sdh`).
4. Provider rating/downloads. Results are grouped by language in preference order.

### 6.3 Flow
- Auto-search on playback (top language missing) → toast *"4 Dutch subtitles found —
  press S"*. No download without confirmation.
- Panel (S): results per provider, quota, preview first lines; Enter downloads.
- Downloaded file: unzip if needed → detect encoding (`chardetng`) → convert to UTF-8
  (`encoding_rs`; Dutch subs are often Windows-1252) → normalise line endings →
  validate it parses → upload to the server (`POST /Videos/{id}/Subtitles`) → refresh
  item streams → select it in mpv.
- If the server upload fails (permission/network): keep in local cache, load into mpv
  locally, show a retry action.
- Non-hash-matched downloads: offer auto-sync (setting `auto_sync_non_hash`, default on).

### 6.4 Sync
- Manual: Z/X ±100 ms, Shift for ±1 s. Stored per item+subtitle in the state DB and
  reapplied; option "apply permanently" rewrites timings and re-uploads.
- Auto: `alass-core` aligns subtitle timespans to a reference — (a) another text sub
  track in the same file (instant), else (b) speech detected in the audio: the audio
  track in the subtitle's language (else the default track) is extracted (server
  audio-only transcode, §7) and run through whisper.cpp's
  `whisper-vad-speech-segments` (Silero VAD, CPU package ~10 MB). Speech spans are
  cached per item+track. Downloads load immediately, re-sync to the audio in the
  background, swap the synced version in and upload that one. Ctrl+Z applies the
  result as a remembered subtitle delay.

### 6.5 Style overrides
Font, size, colour, outline/shadow, bottom margin, background box. Mapped to mpv
`sub-*` options; `sub-ass-override` setting (`no | scale | force`) for ASS files.
Global defaults + per-library overrides (anime libraries usually keep ASS styling).

---

## 7. AI subtitles (`rj-ai`)

Pipeline, run as a cancellable background job with progress:

1. **Audio extraction** — preferred: ask the server for an audio-only transcode of the
   chosen audio stream (16 kHz mono) so a 60 GB remux isn't pulled over the network.
   *Spike:* confirm the right endpoint/params (`/Audio/{id}/stream` or
   `/Videos/{id}/stream` with `audioStreamIndex`, `audioSampleRate=16000`,
   `audioChannels=1`, video disabled). Fallback: headless mpv locally
   (`--vid=no --ao=pcm --audio-samplerate=16000 --audio-channels=mono --untimed`).
2. **Transcription** — official whisper.cpp `whisper-cli` (release b5130, pinned),
   CUDA 12 package on Windows (`ai.whisper_runtime = "cuda11"` / `"cpu"` alternatives),
   JSON output parsed for cue times. Default model `large-v3-turbo` (q5_0). Language =
   audio track language (auto-detect if unknown). Silero VAD on.
3. **Translation, per line** — films mix languages, so Whisper always transcribes
   (never its translate mode, which large-v3-turbo can't do), each line is
   language-detected (`whatlang`, restricted to common languages, uncertain lines
   follow their neighbours or the audio track's language), and translated with
   Opus-MT (MarianMT) via `candle` on CPU: direct en/es/de→nl, anything else via
   `opus-mt-mul-en` then en→nl. Models are fetched only for languages found.
   Safetensors weights from the official conversion-bot PRs, pure-Rust SentencePiece.
4. **Post-process** — max 42 chars/line, 2 lines, min 1 s / max 7 s per cue (longer
   cues are split at a sentence/clause break with time shared by length, not cut
   short), strip repeated hallucination lines.
5. **Save** — upload as `nl` subtitle; record in the state DB as AI-generated (shown with
   an "AI" badge in the track menu, since the server has no field for it).

Triggers (planning prefers translating an existing English text subtitle — better
timing, seconds instead of minutes — and falls back to Whisper):
- Subtitle panel: "Generate Dutch subtitles from audio" (always available).
- When providers return nothing for nl: offer it from the toast.
- When an English sub exists but no nl: also offer the cheaper
  "Translate English subtitle → Dutch" (step 3 only, keeps original timing).
- Batch: "Generate for whole season" from the season's context menu.

Models download on first use into `%LOCALAPPDATA%\redjelly\models` with size shown and
a progress bar. Target: 2 h film transcribed in < 5 min on a recent NVIDIA GPU.

---

## 8. UI screens

Login / user switcher (Quick Connect + password) → Home (Continue watching, Next up,
Latest per library) → Library grid (sort, filter, A–Z jump) → Series → Season →
Episode/Movie detail (versions, audio/sub info, "subs: nl missing" badge) → Player
(OSD, track menus, subtitle panel, AI job progress) → Search → Settings (Server &
users, Playback, Languages, Subtitles & providers, AI, Keybindings, Gamepad,
Appearance) → Jobs (AI queue).

**Discover** (Home toolbar, shown once a Jellyseerr address is set in Settings →
Discover): a wizard (What → Genres include/exclude → Era → Length (movies) → Ratings →
Extras (original language, library/requested, streaming services in a region) →
Sort), remembering the last answers, then a poster grid of TMDb titles via
Jellyseerr's discover API. TMDb score/votes filter server-side; IMDb and Rotten
Tomatoes minimums are checked per title (`/movie/{id}/ratingscombined`,
`/tv/{id}/ratings`; IMDb is movies only), with "checked N · M match" progress,
stopping after 5 rounds without a match. Titles fully in the library open the
Jellyfin page; others open a title page with Request (shows: season picker, unrequested
seasons pre-selected). Jellyseerr sign-in uses the current Jellyfin user's name and
their password from the credential store (one per Jellyfin user), so requests are
attributed and quota-checked per person. Movie and series detail pages append IMDb /
RT scores to the meta line once fetched.
With Jellyseerr set up, Home also has a **Popular & well rated** row (TMDb movies from
the last 2 years, score 7+ with 200+ votes, by popularity), loaded alongside the other
rows and dropped if Jellyseerr is slow (5 s) or fails.

**Requests**: a Home **Requests** row (before Popular) lists open and failed requests
(everyone's when the Jellyseerr account may see them; 8 s budget) with a live status:
waiting for approval, declined/failed, searching / no release yet (after 2 days),
or Radarr/Sonarr's queue state from Jellyseerr's `downloadStatus` (downloading N% ·
time left, queued, paused, delay, importing, stuck/warning, failed, client
unreachable; the worst of several files wins). The same status shows on Discover
cards and title pages. Title pages of requested titles read Radarr/Sonarr's address
and API key from Jellyseerr's settings (needs a Jellyseerr admin account; tries
Jellyseerr's hostname, then the external URL; cached per server) and add:
- a **Download** row with each queue entry's real state and messages;
- **Manual search…**: a side panel with the *arr's interactive search results
  (quality · size · age · seeders · indexer · language · custom format score, rejected
  ones dimmed with reasons, in the *arr's own order); Enter grabs. Shows pick a season
  first (season packs + episodes), or a single episode;
- **Remove & search again** (second press within 4 s confirms): removes the queued
  downloads from the client, blocklists them and starts an automatic search; "Search
  again" when nothing is queued.
Home and title pages refresh silently every 30 s while on screen (no panel open),
keeping already-loaded images.

**Detail page**: tagline, full plot, About (release date, runtime, age/critic
ratings, genres, director, writers, producers, studio, country), Cast & crew with
photos (each opens that person's titles in the library), one card per video/audio/
subtitle track plus the file, and a Trailer button: the server's trailer file, else
the YouTube trailer through mpv's ytdl hook with yt-dlp downloaded on first use into
`%LOCALAPPDATA%\redjelly\data\tools` and refreshed weekly.

Performance targets: cold start to interactive Home < 1 s with warm cache; library
grid scroll at display refresh rate with 10k items (virtualised list, async image
decode); first frame < 1.5 s after Play on LAN.

---

## 9. Milestones

All of these ship in v1; the order is for dependency, not priority.

- **M0 Spikes** — mpv render-API underlay in Slint on Windows (hwdec, HDR); server
  audio-only transcode endpoint; whisper-rs CUDA build on Windows.
- **M1 Core client** — auth, user switcher, home, browse, detail, search, direct-play
  playback, reporting, resume, track selection + memory, focus model, keymap, cheat
  sheet, SMTC.
- **M2 Subtitles** — providers, hash, ranking, panel, encoding fix, upload, delay
  memory, style overrides, alass sync, auto-search toast.
- **M3 AI** — extraction, whisper, Opus-MT translation, job queue, models manager,
  season batch.
- **M4 Polish** — gamepad, keymap editor UI, intro/credits skip, next-episode,
  shader presets, stats overlay, installer.

## 10. Packaging
Cross-built from Linux with `scripts/build-windows.sh` (llvm-mingw, gnullvm target):
- `redjelly-install-<version>.zip` — zipped NSIS 3 installer (browsers block bare unsigned setup .exe downloads) (NSIS from Debian packages,
  unpacked without root): per-user into `%LOCALAPPDATA%\Programs\redjelly`, no UAC,
  Start menu shortcut, optional desktop shortcut, Apps & features entry with
  uninstaller (optionally removes settings and AI models), upgrades in place.
- `redjelly-windows-x64.zip` — the same files as a portable folder.
The exe carries an icon and version info (windres resources); `libmpv-2.dll` from
zhongfly/mpv-winbuild. GUI subsystem; logs in the data dir. Not code-signed, so
SmartScreen warns on first run. Licence GPL-3.0-or-later (LICENSE), notices in
`installer/THIRD-PARTY.txt`; icon generated by `scripts/make-icon.py`.

### 10.1 Releases and self-update
`scripts/release.sh` bumps the patch version, builds, signs the installer exe
with ed25519 (`openssl pkeyutl`, key in `~/.config/redjelly/release-signing.key`
on the build box), tags `v<version>` and publishes a GitHub release with the
exe, its `.sig` and the zip; notes are the commit subjects since the last tag.
The app (`app/src/update.rs`) checks `releases/latest` 10 s after start and
every 12 h, never during playback; Settings ▸ Updates shows the notes and
installs: download, verify against the built-in public key, run the installer
with `/S /RELAUNCH` and quit. The silent installer waits up to 30 s for
redjelly.exe to unlock, then starts the new version. Off with
`[update] auto_check = false`; "Skip this version" stops reminders for it.

## 11. Testing
- Unit: track selection matrix, language normalisation, movie hash, keymap parsing,
  ranking, timing rewrite, encoding conversion.
- HTTP: `wiremock` fixtures for Jellyfin and each provider.
- Integration: dockerised Jellyfin 10.10 with sample media (embedded + external
  subs, multi-audio, forced subs).
