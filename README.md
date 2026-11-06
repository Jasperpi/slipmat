# Slipmat

A free DJ app: your Spotify library on two decks, mixed in a browser page
served from your own machine, played through a Pioneer DDJ controller.

```
slipmat              # start, and open the decks in the browser (http://127.0.0.1:4141)
slipmat --no-open    # start without opening a browser
slipmat doctor       # list sound cards and MIDI ports, check the Spotify login
slipmat doctor --load "song|other song"   # decode + analyse songs, silently
slipmat doctor --fake-ddj                 # pretend to be a DDJ-FLX4 (virtual MIDI) for testing
slipmat doctor --stems clip.f32           # split raw f32 stereo audio into stems, time it
```

Needs Spotify Premium. Slipmat signs in once (or reuses riff's login on the
same machine) and is not a Spotify Connect device: it never touches what the
account is playing elsewhere.

## What it does

- **Library**: Liked Songs, your playlists (with folders), Spotify search.
  BPM and key columns fill in as songs are analysed; *Analyze list* does a
  whole playlist in the background. Keys that mix with the playing deck stay
  bright, BPMs within 6% turn green.
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
  loads (about 10 s per song, on this machine, offline). Drop the drums out
  of one song to bring in the next one's, or play an acapella. Uses Deezer's
  Spleeter 4-stem networks (Apache-2.0, via ONNX Runtime; ~80 MB, downloaded
  to `~/.cache/slipmat/models/` on first use). The stems add back up to the
  original exactly, so with every stem up you hear the untouched song.
  Stems live in memory only. Meta's Demucs separates better but needs over
  a gigabyte of free memory per song, more than this laptop has spare.
- **Sync**: tempo and beat phase, held together while both play; moving a
  synced deck's tempo moves the shared tempo.
- **Mixer**: trim, 3-band isolator EQ (full kill), one-knob filter, channel
  faders, crossfader with curves and A/THRU/B assign, master, headphone
  cue/master blend and level, limiter, meters.
- **FX**: two beat-synced units (echo, reverb, flanger, phaser, roll, trans,
  crush, wobble) on deck 1, deck 2 or the master. On the controller, PAD FX
  modes hold-to-play effects.
- **Controller**: Pioneer DDJ-FLX4, mapped from AlphaTheta's official MIDI
  message list (`research/flx4/`), with LED feedback, channel meters, vinyl
  mode set from Settings, soft takeover on the tempo faders, hot-plugging.
  (The DDJ-400 shares most of the layout.)
- **Sound**: a DDJ is a four-channel sound card: master on 1-2 (the RCA
  outs), headphones on 3-4. Without one, the laptop plays the master and
  *split cue* (Settings) puts the headphone cue in the left ear.

## Controller layout (DDJ-FLX4)

| Control | Does |
|---|---|
| PLAY, CUE | as on a CDJ (hold CUE at the cue point to preview) |
| SHIFT + PLAY | censor (plays backwards, slips back on release) |
| SHIFT + CUE | back to the start |
| Jog top / side | scratch (vinyl mode) / nudge; SHIFT + jog searches |
| BEAT SYNC / SHIFT + BEAT SYNC | sync / change tempo range |
| IN, OUT | loop in / loop out |
| 4 BEAT/EXIT (SHIFT) | 4-beat loop, or exit the loop (back into it) |
| CUE/LOOP CALL ◀ ▶ | halve/double the loop; without a loop, jump 4 beats |
| SHIFT + CUE/LOOP CALL | jump 16 beats |
| SHIFT + IN / OUT | halve / double the loop |
| Pads: HOT CUE, BEAT LOOP, BEAT JUMP, PAD FX1/2 | hot cues (SHIFT deletes), auto loops, jumps, held effects |
| Headphone CUE / SHIFT + CUE | headphone cue / key lock |
| SHIFT + HOT CUE (keyboard mode) pads | stems: 1-4 vocals/melody/bass/drums on-off (SHIFT solos), 5 acapella, 6 instrumental, 7 no drums, 8 all back |
| BEAT FX section | effect unit 1: select, beat ◀ ▶, CH1/CH2/both lever, level/depth, on/off (blinks while on) |
| MASTER LEVEL, MASTER CUE | master volume, master in the headphones |
| HEADPHONE MIX / LEVEL | cue-to-master blend, headphone volume |
| Browse knob, push, LOAD 1/2 | move in the list, switch playlists/songs, load |
| SHIFT + browse knob | zoom the waveforms |

## Not yet

Sampler pads, key shift pads, stem effects, more than two decks, recording (Spotify
doesn't allow recording its streams; Serato blocks it too), MIDI learn for
other controllers.

## Layout

- `src/engine/` the audio engine: decks (`deck.rs`), mixer and sync
  (`mod.rs`), filters (`dsp.rs`), effects (`fx.rs`), sound card (`output.rs`)
- `src/loader.rs` decodes a Spotify song into memory with librespot
- `src/analysis.rs` tempo, grid, key, loudness
- `src/stems.rs` stem separation (STFT → four networks → masks → iSTFT, in 12 s pieces)
- `src/midi.rs` the controller mapping and lights
- `src/server.rs`, `src/app.rs` the local web server and shared state
- `src/spotify/` sign-in, library and metadata, shared with riff
- `ui/` the page (plain JS modules, no build step)

Build: `cargo build --release -j3`. Tests: `cargo test`.
`SLIPMAT_NO_AUDIO=1` runs the decks without a sound card (for testing).
