# Agent Instructions

- After finishing code or documentation updates, give the user a git commit message.
- When bumping the Small Harness version or shipping a user-visible release, do not stop after updating `Cargo.toml`, `Cargo.lock`, `README.md`, and `CHANGELOG.md`.
- For each release bump, push the release commit, create and push the matching `vX.Y.Z` tag, watch the GitHub release workflow, and verify that the GitHub release exists with macOS assets.
- After the release workflow finishes, verify `GetSmallAI/homebrew-tap` has `Formula/small-harness.rb` updated to the same version. If practical, also test `brew upgrade small-harness` from outside this repo.
