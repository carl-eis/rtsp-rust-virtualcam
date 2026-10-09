#!/bin/sh
# Fails if an OS `cfg` appears outside the places the cross-platform plan allows
# (documentation/09-cross-platform-plan.md, "Where cfg may appear"):
#   - rtspcam-platform, rtspcam-vcam, rtspcam-vcam-mgr (all of them)
#   - rtspcam-cli: the `vcam` subcommand gating in main.rs and Cargo.toml
#   - `windows_subsystem` in rtspcam-app (packaging; ignored on other OSes)
# Run from the repository root.
set -eu

pattern='cfg(_attr)?[[:space:]]*\(.*\b(windows|unix|target_os|target_family|target_env|target_vendor)\b'

found=$(git grep -nE "$pattern" -- 'crates/*.rs' 'crates/*Cargo.toml' \
    ':!crates/rtspcam-platform/' \
    ':!crates/rtspcam-vcam/' \
    ':!crates/rtspcam-vcam-mgr/' \
    ':!crates/rtspcam-cli/src/main.rs' \
    ':!crates/rtspcam-cli/Cargo.toml' | grep -v 'windows_subsystem' || true)

if [ -n "$found" ]; then
    echo "OS-specific cfg outside rtspcam-platform (move it behind a platform trait):"
    echo "$found"
    exit 1
fi
echo "No OS-specific cfg outside the allowed crates."
