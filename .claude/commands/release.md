---
description: Cut a release - analyse changes since the last tag, bump the version, commit, tag, push to master and publish GitHub release notes
argument-hint: "[patch|minor|major] (optional; overrides the inferred bump)"
allowed-tools: Bash(git:*), Bash(gh:*), Bash(cargo:*), Bash(grep:*), Bash(sed:*), Read, Edit, Grep, Glob
---

Cut a new release of this repo. Work through the steps in order and stop to ask the user if
anything looks wrong (failing build, unexpected branch, ambiguous bump, conflicts on push).

Requested bump override: `$ARGUMENTS` (empty means infer it in step 2).

## 1. Analyse the changes since the last release

- `git fetch origin --tags` so the tags and `origin/master` are current.
- Find the last release tag: `git tag --sort=-v:refname | head -1` (tags look like `v1.2.1`).
- Confirm you are on `master` (`git branch --show-current`) and that it is not behind
  `origin/master`. If you are on another branch, or in a worktree whose branch is not master,
  stop and ask the user how they want to proceed (merge the branch first, or release from it).
- Review what changed: `git log <last-tag>..HEAD --oneline --no-merges`, the merged PRs in
  `git log <last-tag>..HEAD --merges --oneline`, `git diff <last-tag>..HEAD --stat`, and the
  actual diffs of anything whose effect on users isn't clear from the commit message.
- Include uncommitted work too: `git status` and `git diff` / `git diff --cached`. These changes
  are committed in step 3 and are part of the release.
- If there is nothing new since the last tag, stop and tell the user.

## 2. Decide the version and bump it

The version follows semver and lives in one place: `version` under `[workspace.package]` in the
root `Cargo.toml`. Every crate inherits it with `version.workspace = true`; the Windows installer
gets it from the build script, so `installer/rtspcam.iss` needs no edit.

Pick the bump (unless `$ARGUMENTS` names one):

- **patch** (default): fixes, CI/test changes, docs, refactors, dependency updates, small
  behaviour tweaks.
- **minor**: a user-visible feature was added (new setting, new platform support, new CLI
  command, new protocol capability, etc.).
- **major**: only if there is a breaking change for users (config format no longer read,
  installer can't upgrade over the previous major, IPC/DLL incompatibility). Confirm with the
  user before doing a major bump.

State the chosen version and a one-line reason before editing.

Then update every place that names the current version:

- `Cargo.toml`: `[workspace.package] version`.
- `Cargo.lock`: refresh with `cargo update --workspace` (only touches the workspace crates'
  versions; check `git diff Cargo.lock` shows nothing else changed).
- `README.md`: the "Status" section (`Early development; version X.Y.Z.`).
- `documentation/00-index.md`: the heading `## 1. Where the project stands (as of <date>, vX.Y.Z)`;
  update the date to today as well.
- Finally grep for any other occurrence of the old version and judge each hit:
  `git grep -n "<old-version>" -- ':!Cargo.lock'`. Leave historical references (changelogs,
  links to older releases) alone.

Run `cargo check --workspace --locked` to make sure the lockfile and manifests agree.

## 3. Commit

- Stage the version bump together with any uncommitted changes found in step 1. Look at the
  untracked files before staging them; never commit secrets, build output, or scratch files.
- If the uncommitted changes are real work (not just the bump), commit them first in their own
  commit with a conventional message describing them, then commit the bump separately.
- The bump commit message is exactly `chore: release vX.Y.Z` (matches previous releases).

## 4. Tag

- Create an annotated tag on the release commit: `git tag -a vX.Y.Z -m "vX.Y.Z"`.
- The release workflow (`.github/workflows/release.yml`) fails if the tag doesn't match the
  `Cargo.toml` version, so double-check they agree.

## 5. Push and create the GitHub release

- Push master, then the tag: `git push origin master` and `git push origin vX.Y.Z`. Pushing the
  tag starts the release workflow, which builds the Windows installer and the Linux `.deb` /
  `.rpm` and attaches them to the release for this tag.
- Write the release notes to a temp file and create the release:
  `gh release create vX.Y.Z --title "vX.Y.Z" --notes-file <file> --verify-tag`.
- Follow the style of previous releases (`gh release view <last-tag>` to see one):
  - A one-line summary (e.g. "A small fix release on top of 1.2.0.").
  - `## Features` and/or `## Fixes` sections with bold-led bullets written for users, explaining
    what was wrong or what is new rather than listing commits. Group CI/test/docs changes into a
    short final bullet or omit them if trivial.
  - `## Upgrading`: Windows (install over any 1.x; say whether config, the camera DLL and the
    pipe protocol are unchanged) and Linux (install the new `.deb` / `.rpm` over the previous
    version). Call out anything that changes this.
  - `## Downloads`: `RtspCam-X.Y.Z-setup.exe` for Windows, `.deb` and `.rpm` for Linux (x86_64,
    glibc 2.35+), macOS builds from source per the README; note the workflow attaches them when
    its builds finish.
  - `**Full changelog:** https://github.com/carl-eis/rtsp-rust-virtualcam/compare/<last-tag>...vX.Y.Z`
- Report back: the new version and why, the commits made, the release URL, and the release
  workflow run (`gh run list --workflow release.yml --limit 1`).
