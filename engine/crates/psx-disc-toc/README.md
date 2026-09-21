# Collection catalog contract

This core-only crate is the single owner of the existing PSXDEMO1 byte layout used by both the demo disc and Arcade. The extraction preserves every field offset, constant, checksum operation and test. Host packers and guest launchers depend on the same package; format changes require updating both consumers together and retaining backward-compatibility fixtures. This move introduces no new format version.
