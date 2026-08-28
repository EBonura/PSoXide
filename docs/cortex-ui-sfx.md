# Cortex Ignition UI SFX inventory

The temporary UI library uses shortened and processed mechanical recordings so
repeated menu navigation stays tactile without masking music or game audio.
Sources are mono 44.1 kHz PCM WAV and are converted to PS1 SPU ADPCM by the
normal UI cooker.

| Cue | Role | Intended binding |
| --- | --- | --- |
| `ui_navigate` | Restrained relay click | Focus |
| `ui_confirm` | Rising mechanical acknowledgement | Button press |
| `ui_back` | Falling release gesture | Back / dismiss |
| `ui_tab_shift` | Three-stage lateral mechanism | L1/R1 tab change |
| `ui_slider_tick` | Very short encoder detent | Slider nudge |
| `ui_limit` | Muted double refusal | Slider limit / invalid action |
| `ui_socket` | Clamp followed by an energy lock | Assign module |
| `ui_unsocket` | Mechanical release and falling discharge | Remove module |

An item-acquired cue and the message/menu transition cues remain intentionally
unfilled until their final sounds are approved.

The current placeholders are transformed from Brian MacIntosh's **Mechanical
Sounds** pack, released under CC0 1.0:

<https://opengameart.org/content/mechanical-sounds>

The transformations shorten the recordings, reshape pitch, filter their
frequency range, compress the transient and apply restrained bit-depth
reduction. See the `SOURCE.md` beside each project copy for the source mapping.

The editor discovers every WAV under a project's `assets` folder. Select a
button or slider, expand **SFX**, choose a cue, and use **Preview** to audition
it before cooking.

To populate only empty UI bindings while preserving every authored override:

```sh
python3 tools/install_cortex_ui_sfx.py path/to/project.ron
```
