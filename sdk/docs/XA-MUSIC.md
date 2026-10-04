# XA-ADPCM music

CD-DA costs 2352 bytes per sector at 75 sectors a second whatever is on it.
XA-ADPCM squeezes the same stereo song into a quarter of that (or an eighth),
and the drive decodes it in hardware, so the CPU does no work. A game that
ships several programs or levels on one disc usually runs out of disc before
it runs out of ideas for music; this is how to get that space back.

The pieces, all in this repository:

| What | Where |
| --- | --- |
| Encoder, reference decoder, interleaver | `psx-audio-cook xa-encode`, `xa-decode`, `xa-score` (`crates/psx-audio-cook/src/xa.rs`) |
| Disc builder | `mkisopsx --xa-file` (`crates/psx-iso`) |
| Player | `psx_io::cd::xa` (`Player`) |
| Example | `sdk/examples/hello-xa`, built by `make hello-xa-disc` |

## Converting CD-DA tracks

Start from WAV files, 16-bit PCM or better, any rate, mono or stereo. The
encoder resamples with a Kaiser-windowed sinc (the same one the SPU cooker
uses) and shapes the channels to the target format.

```sh
cargo run --release -p psx-audio-cook -- xa-encode MUSIC.XA \
    title.wav level1.wav level2.wav boss.wav --manifest music.json
cargo run --release -p mkisopsx -- --exe game.exe --out game.bin \
    --xa-file MUSIC.XA        # was: --cdda-track title.raw --cdda-track ...
```

The songs become channels 0, 1, 2 and 3 in the order given. `mkisopsx` prints
the file's LBA and length, writes each sector as Mode 2 Form 2 with its EDC,
and the file shows up in the root directory like any other. Options of
`xa-encode`:

| Option | Default | Meaning |
| --- | --- | --- |
| `--rate 37800\|18900` | 37800 | Output sample rate. |
| `--mono` | stereo | One channel per song (a stereo source is averaged). |
| `--speed 1\|2` | 1 | Drive speed the interleave is built for. |
| `--file N` | 1 | File number in every subheader (the player filters on it). |
| `--peak F` | off | Scale the loudest sample to this fraction of full scale. |
| `--manifest PATH` | none | JSON with the file number, speed, stride and each song's channel and length. The same JSON goes to stdout. |

Check what you encoded before burning it:

```sh
psx-audio-cook xa-score title.wav MUSIC.XA 0     # {"snr_db":[...]} per channel
psx-audio-cook xa-decode MUSIC.XA 0 check.wav    # listen to the reference decode
```

`xa-score` compares the reference decoder's output with the source resampled
to the file's rate. A tonal test chord at 37.8 kHz stereo scores above 45 dB;
dense modern mixes land lower, as any 4-bit codec does.

To hear it without a console, `make hello-xa-disc` builds a disc with four
generated songs, and `make hello-xa-gate FRONTEND=<headless emulator>` plays
it, presses CROSS three times and checks the emulator's audio capture and
drive command log (`tools/xa_gate.sh`).

## Choosing a format

The drive hands one sector in every `stride` to the decoder, and the stride
is fixed by the sample format and the drive speed. A song in a sparser file
than that plays too slowly, in a denser one too fast.

| Format | Stride at single speed | Stride at double speed |
| --- | --- | --- |
| 37.8 kHz stereo | 4 | 8 |
| 18.9 kHz stereo, or 37.8 kHz mono | 8 | 16 |
| 18.9 kHz mono | 16 | 32 |

A file holds up to `stride` songs (and at most 32 channels). **Fill the
stride.** Slots with no song hold filler sectors that cost disc space exactly
like music: one 37.8 kHz stereo song alone in a single-speed file takes as
much disc as CD-DA does. The encoder warns when slots are empty. All songs in
a file get the same length (shorter ones are padded with silence), so group
songs of similar length.

Single speed is the default because it fills a file with fewer songs and the
drive then reads no faster than the music needs. A game that
reads data at double speed changes the drive's speed when music starts, and
the drive needs a moment for that, so choose the speed the game spends most of
its time in and build the file for it.

### Size of a three-minute stereo song

Per song, in a file whose stride is full. Disc sectors are 2352 bytes.

| Source | Sectors | Bytes | Share of CD-DA |
| --- | --- | --- | --- |
| CD-DA, 44.1 kHz stereo | 13500 | 31,752,000 | 100% |
| XA 37.8 kHz stereo | 3375 | 7,938,000 | 25% |
| XA 18.9 kHz stereo | 1688 | 3,970,176 | 12.5% |

Add the 16-sector guard at the end of each file, and a long file's audio
bytes are 2304 of every 2352 (the rest is sync, header, subheader and EDC).

## Playing

```rust
use psx_io::cd::xa::{DriveSpeed, Event, File, Player};

// Once: the SPU must take the drive's audio.
psx_spu::enable_cd_audio(true);
psx_spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);

// Find MUSIC.XA by name (psx_fmv::iso has a root directory lookup) and
// describe it with the numbers from the manifest.
let music = File::from_directory_entry(lba, size_bytes, 1, DriveSpeed::Single);
let mut player = Player::new(peripherals.cd);

player.set_volume(0x80, 0x80);          // drive mixer, 0x80 is unity
player.play(music.song(2), true)?;      // channel 2, looping
loop {
    if player.poll() == Event::Looped { /* optional */ }
    // ...
}
player.stop();                           // pause; play() starts again from the top
```

`Player` owns the CD token, so nothing else drives the controller while it
exists; `release()` stops the music and returns the token.

`play` sends Demute, Setmode, Setfilter, Setloc and ReadS (a few
milliseconds), starting from the top of the file. Playing another song is the
same call. `poll` should run about once a frame: it asks the drive where the
head is, and at the end of the song file it restarts (looping) or pauses the
drive. `elapsed_millis` reports the head's position since the start, a sector
or two ahead of the sound.

`LBA`s are relative to the program's own disc image, so a program chain-loaded
from a multi-program disc finds its music through `psx_io::disc_base` as it
does its other data.

## Trade-offs

**The drive is busy while music plays.** XA music is a read: the laser follows
the music file, so the CPU cannot read other data from the disc meanwhile,
not even at the same speed. Load first and then start the song, or stop the
music for a load and `play` it again afterwards (it restarts from the top).
Games that stream levels from the disc cannot use XA music for the stream's
duration. Streaming data and music together needs the data interleaved into
the filler slots, which neither the encoder nor the player does. CD-DA has
the same restriction, so a game that already works around CD-DA reads loses
nothing.

**Looping seeks.** A loop is a seek back to the start of the file, so the
music has a gap as long as that seek: about a fifth of a second in the
emulator, and a drive mechanism's real seek time on a console (not measured
yet). Songs that loop should end and begin in quiet passages. CD-DA looping
has the same gap, so existing music already hides it.

**Quality.** The encoder searches all four filters and thirteen shifts for
each block of 28 samples and keeps the pair with the least error against the
decoder's own history, so a tonal test chord round-trips above 45 dB. Real
mixes score lower and carry the usual 4-bit ADPCM hiss on quiet passages.
18.9 kHz keeps nothing above 9.4 kHz.

**One file number per file.** All songs of a file share the file number, so
the player filters on the channel. Several files can sit on one disc with
different file numbers.

## What the encoder writes

Every sector is audio, Form 2 and real time (submode `0x64`), with coding
info `0x01` (37.8 kHz stereo), `0x05` (18.9 kHz stereo), `0x00` or `0x04`
(mono). Song sector `k` of channel `c` sits at file offset `k * stride + c`.
Unused slots and the guard at the end are audio-typed filler sectors on
channel `0xFF`, which the drive ignores while the filter is on (it neither
decodes them nor raises a data interrupt). The last sector of the file has the
end-of-file and end-of-record bits. The first block of every channel uses
filter 0, so a restart or a seek into the song decodes from cold.

There is no loop marker in the data: the format has only end-of-file, and the
player loops by watching the head position. The encoder writes 4-bit samples
without emphasis; 8-bit ADPCM is not supported.
