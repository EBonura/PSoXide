# Verified itch.io publisher

This composite action is the common publication boundary for Bonnie Studios
repositories. Each caller keeps its own build and packaging steps, then passes
one flat staged directory here.

The action validates the repository-to-channel mapping, version, package path,
contents and symlink policy before it installs Butler. `publish: false` performs
all package checks without needing a credential or contacting itch.io.

Consumers must pin this action by its full commit SHA and provide
`BUTLER_API_KEY` through the caller job environment only when publishing.
