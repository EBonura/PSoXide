# Provenance Checklist

A practical checklist to run before adding code or assets to PSoXide.
PSoXide is GPL-2.0-or-later and is not a clean-room project (see
[`downstream-licensing.md`](downstream-licensing.md)); the point of this
checklist is to keep provenance explicit and the GPL story coherent, not to
pretend influence away.

Use it for every non-trivial addition, whether written by a human, by an AI
assistant, or copied/adapted from elsewhere.

## For code

- [ ] **Origin.** Is this code copied, translated, adapted, AI-generated, or
      original? Be honest about which. "Translated from C++ to Rust" is a
      derivation, not original work.
- [ ] **License.** If it is copied/translated/adapted, does the source have a
      license? No license means it is not yours to use; stop.
- [ ] **GPL compatibility.** Is that license compatible with GPL-2.0-or-later?
      (MIT, BSD, Apache-2.0, MPL-2.0, LGPL, and GPL-2.0+ are fine. CC-BY-NC,
      CC-BY-ND, "no commercial use", and proprietary are not.) If unsure,
      treat it as incompatible until confirmed.
- [ ] **Attribution.** Does the license require attribution or notice
      preservation? If so, add it; never strip copyright headers.
- [ ] **Mark derivation, do not disguise it.** If this is based on another
      emulator or source project, add a `## Provenance` header naming the
      project, its copyright holders, and its license, with inline markers at
      the points of correspondence. Never present derived code as clean-room
      or original.
- [ ] **AI-assisted additions.** If an AI assistant wrote it, were the
      references/prompts rights-clean? AI assistance is not clean-room; if the
      output looks like it reproduces a specific known source, treat it as
      derived and check that source's license.
- [ ] **Dependencies.** If this adds a crate dependency, does
      `cargo deny check licenses` still pass across every workspace?

## For assets

- [ ] **Origin.** Original, commissioned, licensed stock, AI-generated, or
      copied? Record which.
- [ ] **License and rights.** What license or terms cover it? For
      AI-generated assets, do the service's terms grant the rights you need,
      and were the inputs rights-clean?
- [ ] **Source URL / evidence.** Is the exact source URL (or invoice/export
      evidence for paid or generated assets) recorded in
      [`asset-provenance.md`](asset-provenance.md)? Capture it at the time; it
      is hard to reconstruct later.
- [ ] **Generated binaries are traceable.** Cooked/baked binary blobs inherit
      the license of their source material. Is the source material identified,
      so the blob's license is traceable?
- [ ] **Distribution rights.** For anything provided locally or of unclear
      origin, are distribution rights confirmed before it ships in a public or
      commercial build? If not, mark it unresolved in `asset-provenance.md`
      rather than shipping it quietly.

## When in doubt

Document the uncertainty rather than guessing. An item recorded as "source URL
not captured" or "rights to be confirmed" is honest and actionable; an
invented URL or an unstated assumption is neither.
