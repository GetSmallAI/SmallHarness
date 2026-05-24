# Homebrew formula template for Small Harness.
#
# How to publish:
#
#   1. Tag a release in the SmallHarness repo (e.g. `git tag v0.3.0 && git
#      push --tags`). The release workflow at
#      .github/workflows/release.yml builds aarch64 + x86_64 tarballs and
#      attaches them to the GitHub release.
#   2. Copy this file into a separate tap repo named
#      `getsmallai/homebrew-tap`, under `Formula/small-harness.rb`.
#   3. Bump `VERSION`, `ARM64_SHA256`, and `X86_64_SHA256` below to match the
#      published release. Tarball checksums are in the release's SHA256SUMS
#      file (the release workflow generates one).
#   4. Users install with `brew install getsmallai/tap/small-harness`.
#
# Notes:
#   - This formula installs a pre-built binary, not a Cargo build, so users
#     don't need a Rust toolchain to install.
#   - If you decide later to submit to homebrew-core instead, the formula
#     shape mostly carries over; homebrew-core may require a from-source
#     build path as well.

class SmallHarness < Formula
  desc "Terminal-based agent harness for running small LLMs on your Mac"
  homepage "https://github.com/GetSmallAI/SmallHarness"
  version "0.3.0" # bump per release
  license "MIT"

  ARM64_SHA256 = "0000000000000000000000000000000000000000000000000000000000000000".freeze
  X86_64_SHA256 = "0000000000000000000000000000000000000000000000000000000000000000".freeze

  on_macos do
    on_arm do
      url "https://github.com/GetSmallAI/SmallHarness/releases/download/v#{version}/small-harness-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 ARM64_SHA256
    end
    on_intel do
      url "https://github.com/GetSmallAI/SmallHarness/releases/download/v#{version}/small-harness-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 X86_64_SHA256
    end
  end

  def install
    bin.install "small-harness"
    # README/LICENSE/Quickstart are bundled in the tarball — install them
    # under doc/ so `brew info` and `brew docs` show something useful.
    doc.install Dir["README.md", "Quickstart.md", "LICENSE"]

    # Best-effort shell completion install. `small-harness completions <shell>`
    # writes the script to stdout; pipe it into the standard locations
    # Homebrew already manages.
    generate_completions_from_executable(bin/"small-harness", "completions")
  end

  test do
    assert_match "small-harness", shell_output("#{bin}/small-harness completions bash")
  end
end
