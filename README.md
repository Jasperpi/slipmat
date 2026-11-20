# Slipmat

A free DJ app: your Spotify library on two decks, mixed in a browser page
served from your own machine, played through a Pioneer DDJ controller.

```
slipmat              # start, and open the decks in the browser (http://127.0.0.1:4141)
slipmat --no-open    # start without opening a browser
slipmat doctor       # list sound cards and MIDI ports, check the Spotify login
slipmat doctor --load "song|other song"   # decode + analyse songs, silently
slipmat doctor --fake-ddj                 # pretend to be a DDJ-FLX4 (virtual MIDI) for testing
slipmat doctor --stems clip.f32 [--model demucs]   # split raw f32 stereo audio into stems, time it
```

Needs Spotify Premium. Slipmat signs in once (or reuses riff's login on the
same machine) and is not a Spotify Connect device: it never touches what the
account is playing elsewhere.

## On a Mac

Apple Silicon or Intel, macOS 12 or later. In Terminal:

```
git clone git@github.com:Jasperpi/slipmat.git && cd slipmat && sh scripts/mac-install.sh
```

The script installs Rust if needed (Apple's command line tools must be there:
`xcode-select --install`), builds Slipmat (a few minutes) and puts a
**Slipmat** app in `~/Applications`. The FLX4 needs no driver on a Mac.
On a Mac with 16 GB, stems use Demucs automatically.

## On Windows

Slipmat builds and runs on Windows too (type-checked for
`x86_64-pc-windows-msvc`; not yet run on a real Windows machine).

1. Install Rust from <https://rustup.rs> (take the default; it offers the
   Visual Studio C++ build tools it needs).
2. Get this folder onto the PC (`git clone`), then in it run
   `powershell -ExecutionPolicy Bypass -File scripts\windows-install.ps1`.
   It builds Slipmat (a few minutes) and adds Start menu and desktop
   shortcuts. Closing Slipmat's console window quits it.
   Alternatively, run the *Builds* action on GitHub (Actions → Builds → Run
   workflow) for a ready-made zip (`slipmat.exe` + `DirectML.dll`).

On a 16 GB PC, stems use Demucs automatically, on the graphics card through
DirectML. Close rekordbox before starting Slipmat: on Windows only one app can
use the controller's MIDI at a time. If the sound pill says "no cue out",
set the DDJ-FLX4 to 4 channels (Quadraphonic) in Windows sound settings.

## What it does

- **Auto-DJ**: drop songs in the Auto-DJ list (or press **+**, or *Auto-DJ
  list* for a whole playlist) and turn it on. It plays each song and mixes
  into the next like a DJ: next song waiting at its first downbeat, started
  on a phrase near the end, synced to the beat, faded in with its bass cut,
  basslines swapped halfway, and (with stems) its vocals held back until the
  old song is gone; afterwards it eases back to its own tempo. *Mix now*
  mixes at the next bar; mixes last 8, 16 or 32 bars.
- **Keyboard**: the whole thing can be played from the laptop. Left hand
  runs deck 1, right hand deck 2 (press **?** for the map).

- **Library**: Liked Songs, your playlists (with folders), Spotify search.
  BPM and key columns fill in as songs are analysed; *Analyze list* does a
  whole playlist in the background. Keys that mix with the playing deck stay
  bright, BPMs within 6% turn green, and **Match** shows only the songs that
  fit the playing deck. Lists are kept on disk, so Liked Songs show at once
  when Slipmat starts (then refresh).
- **Decks**: drag a song on, or highlight it and press LOAD. Each song is
  downloaded whole (1-2 s) so cueing, looping and scratching can jump anywhere.
  CDJ-style CUE, 8 hot cues, auto loops ¼-32 beats, loop in/out/reloop,
  halve/double, beat jump, tempo fader (±6/8/10/16/50%), key lock
  (time-stretching), slip, censor (SHIFT+PLAY), jog-wheel scratch and nudge.
- **Analysis**: steady-tempo beat grid placed on the kicks (folded into
  78-156 BPM like Serato's default), key (Camelot or names), loudness for
  auto gain, auto cue on the first beat. Grid fixes per deck (nudge, set beat 1,
  BPM ±0.05, ×2, /2). Everything is remembered per song in
  `~/.local/share/slipmat/tracks/`.
- **Stems**: every song is split into vocals, melody, bass and drums as it
  loads, offline. *Fast* is Spleeter (about 10 s a song, 400 MB); *Best* is
  Meta's HT-Demucs (much cleaner, about 1 GB, on the GPU on Windows);
  *Automatic* picks Demucs on computers with 12 GB or more. Drop the drums out
  of one song to bring in the next one's, or play an acapella. Uses Deezer's
  Spleeter 4-stem networks (Apache-2.0, via ONNX Runtime; ~80 MB, downloaded
  to `~/.cache/slipmat/models/` on first use). The stems add back up to the
  original exactly, so with every stem up you hear the untouched song.
  Stems live in memory only. The big waveforms show each song as its stems
  in colour (bass, melody, vocals, drums from the centre out); a muted stem
  fades to a ghost.
- **Sync**: tempo and beat phase, held together while both play; moving a
  synced deck's tempo moves the shared tempo.
- **Mixer**: trim, 3-band isolator EQ (full kill), one-knob filter, channel
  faders, crossfader with curves and A/THRU/B assign, master, headphone
  cue/master blend and level, limiter, meters.
- **FX**: two beat-synced units (echo, low cut echo, ping pong, spiral,
  reverb, flanger, phaser, roll, trans, crush, wobble) on deck 1, deck 2 or
  the master. On the controller, PAD FX modes hold-to-play effects.
- **Smart CFX**: the FILTER knobs add an effect on top of the filter as they
  turn (dub echo, space, noise, crush or jet).
- **Smart Fader**: moving from one deck to the other (crossfader or channel
  faders) also glides the tempo from one song's to the other's and swaps
  their basslines around the middle.
- **Key shift and key sync**: move a song's key by semitones without
  changing its tempo; *MATCH* shifts it the few semitones that make it fit
  the other deck.
- **Sampler**: sixteen one-shot pads. Eight sounds are built in (air horn,
  siren, laser, kick, clap, snare, riser, boom); put `1.wav` … `16.wav` in
  `~/.config/slipmat/samples/` for your own (pads 9-16 are deck 2's).
- **Memory cues**: up to eight saved spots per song (with their loop, if
  one was on), stepped through with CUE/LOOP CALL.
- **Instant doubles**: press LOAD twice (or *DBL*) to put the other deck's
  song on this one at the same spot and tempo.
- **Loop rolls and fader start**: SHIFT + a BEAT LOOP pad loops while held,
  then the song carries on where it would have been; SHIFT + a fader up from
  the bottom starts its deck.
- **History**: every song the room has heard for 20 seconds is logged
  (*History* in the library) and marked *played* in the lists.
- **Controller**: Pioneer DDJ-FLX4, mapped from AlphaTheta's official MIDI
  message list (`research/flx4/`), with LED feedback, channel meters, vinyl
  mode set from Settings, soft takeover on the tempo faders, hot-plugging.
  (The DDJ-400 shares most of the layout.)
- **Smooth on a busy laptop**: the sound thread runs at real-time priority
  (RTKit on Linux, Mach on macOS, MMCSS on Windows) and stem separation and
  analysis at low priority. Mixing uses about 1% of the time it has, even
  while a song is being split.
- **Sound**: a DDJ is a four-channel sound card: master on 1-2 (the RCA
  outs), headphones on 3-4. Slipmat moves onto it when it is plugged in
  (before or after starting) and back off when it is pulled out; on Linux it
  goes through PipeWire when PipeWire runs the card. If the headphone cue
  has no output of its own, a red bar says so (*Reconnect sound* looks
  again). Without a controller, the laptop plays the master and *split cue*
  (Settings) puts the headphone cue in the left ear.
- **Hard to knock over**: a download Spotify drops is retried, the last song
  asked for wins if several are loaded in a hurry, a deck that is playing is
  never loaded over, a synced deck waiting at its cue stays exactly on it,
  and the controller's lights are sent a few at a time and repeated, so one
  lost message never leaves a light wrong.

## Controller layout (DDJ-FLX4)

| Control | Does |
|---|---|
| PLAY, CUE | as on a CDJ (hold CUE at the cue point to preview) |
| SHIFT + PLAY | censor (plays backwards, slips back on release) |
| SHIFT + CUE | back to the start |
| Jog top / side | scratch (vinyl mode) / nudge; SHIFT + jog searches |
| BEAT SYNC (hold) / SHIFT + BEAT SYNC | sync on/off (hold: on) / change tempo range |
| IN, OUT | loop in / loop out |
| 4 BEAT/EXIT (SHIFT) | 4-beat loop, or exit the loop (back into it) |
| CUE/LOOP CALL ◀ ▶ | in a loop: halve/double it; else previous/next memory cue (none saved: jump 4 beats) |
| SHIFT + CUE/LOOP CALL ◀ / ▶ | delete a memory cue / save one |
| SHIFT + IN / OUT | halve / double the loop |
| HOT CUE pads | 8 hot cues (SHIFT deletes) |
| PAD FX 1 / PAD FX 2 pads | effects while held (roll, echo, trans, wobble, reverb, flanger…) |
| BEAT JUMP pads | ∓1, 2, 4, 8 beats (SHIFT: sixteen times as far) |
| SAMPLER pads | the sampler's sounds: deck 1's pads 1-8, deck 2's 9-16 (SHIFT stops one) |
| KEYBOARD pads (SHIFT + HOT CUE) | stems: 1-4 vocals/melody/bass/drums on-off (SHIFT solos), 5 acapella, 6 instrumental, 7 no drums, 8 all back |
| BEAT LOOP pads (SHIFT + BEAT JUMP) | loops of ¼ to 32 beats (SHIFT: a loop roll while held) |
| KEY SHIFT pads (SHIFT + SAMPLER) | top row −2 −1 +1 +2 semitones (SHIFT −12 −7 +7 +12); bottom row: match the other deck, down, up, reset |
| Headphone CUE / SHIFT + CUE | headphone cue / key lock |
| CH FADER, TRIM, HI/MID/LOW | as marked |
| SHIFT + CH FADER or crossfader | fader start: up from the bottom plays, back down returns to the cue |
| CFX knobs / SMART CFX (SHIFT) | filter / filter + an effect (next effect) |
| SMART FADER (SHIFT) | faders blend tempo and bass too (Auto-DJ on/off) |
| BEAT FX section | effect unit 1: select, beat ◀ ▶, CH1/CH2/both lever, level/depth, on/off (blinks while on) |
| SHIFT + BEAT ◀ / ▶ | effect back to one beat / effect on the master |
| MASTER LEVEL, MASTER CUE (SHIFT) | master volume, master in the headphones (quantize on/off) |
| HEADPHONE MIX / LEVEL | cue-to-master blend, headphone volume |
| Browse knob, push | move in the list, switch playlists/songs |
| LOAD 1/2 (twice) (SHIFT) | load (instant double of the other deck) (sort the list by BPM / key) |
| SHIFT + browse knob | zoom the waveforms |
| MIC LEVEL | the microphone is mixed inside the controller; nothing for Slipmat to do |

## Not yet

Effects on single stems, more than two decks (the FLX4 has two channels),
MIDI learn for other controllers, video, DVS. Recording is left out on
purpose: Spotify doesn't allow recording its streams, and rekordbox and
Serato block it for streaming songs too.

## Layout

- `src/engine/` the audio engine: decks (`deck.rs`), mixer and sync
  (`mod.rs`), filters (`dsp.rs`), effects (`fx.rs`), sampler (`sampler.rs`),
  sound card (`output.rs`)
- `src/loader.rs` decodes a Spotify song into memory with librespot
- `src/analysis.rs` tempo, grid, key, loudness
- `src/autodj.rs` Auto-DJ: when and how to mix into the next song
- `src/priority.rs` real-time / background thread priorities
- `src/stems.rs` stem separation (STFT → four networks → masks → iSTFT, in 12 s pieces)
- `src/midi.rs` the controller mapping and lights
- `src/server.rs`, `src/app.rs` the local web server and shared state
- `src/spotify/` sign-in, library and metadata, shared with riff
- `ui/` the page (plain JS modules, no build step)

Build: `cargo build --release -j3`. Tests: `cargo test`.
`SLIPMAT_NO_AUDIO=1` runs the decks without a sound card (for testing);
`SLIPMAT_PORT=4242` runs a second copy beside the installed one.
`slipmat doctor --fake-ddj --name "AlphaTheta test rig" --midi "97 30 7F, wait 300, show 97 30"`
plays a controller: sends those messages and prints lights back.
