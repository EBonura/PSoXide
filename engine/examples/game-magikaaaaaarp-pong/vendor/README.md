# magikAAAAArp Pong Texture Source

`magikaaaaaarp_album.jpg` is the source image used for the cube ball in
`game-magikaaaaaarp-pong`.

`score_flyby.png` is the source image used for the score flyby gag. It keeps
transparent pixels outside the circular cutout.

The runtime embeds `../assets/magikaaaaaarp_album.psxt`, cooked from this
image through `make assets`, and `../assets/score_flyby.psxt` cooked from
the flyby source.

`../assets/goncharov_spectrum_16x30hz.bin` is a baked 16-band,
30 Hz spectrum visualizer table generated from `assets/audio/cdda/GONCHAROV.wav`
with `make magikaaaaaarp-pong-spectrum`.

The mixed-mode disc uses `assets/audio/cdda/GONCHAROV.track02.cdda`,
shared with the CD-DA streaming demo.
